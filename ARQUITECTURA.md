# ARQUITECTURA — extractor_miniaturas

Addon de Node (napi-rs) escrito en Rust que genera **miniaturas PNG** desde
dos formatos de entrada: PDF (`.pdf`) y Word
(`.docx`/`.docm`).

## Principios

- **Separación de responsabilidades**: un módulo por formato y un módulo por
  renderizador; el orquestador solo despacha por extensión. Ningún extractor
  sabe cómo se renderiza y ningún renderizador sabe de dónde vienen los datos.
- **Código auxiliar dentro de structs**: toda la lógica vive en `impl` de
  structs; no hay funciones sueltas fuera de clases (regla del proyecto).
- **Errores como `String`** internamente, traducidos a `napi::Error` solo en
  la frontera con Node.

## Estructura general

```
src/lib.rs               Orquestador + funciones exportadas a Node (#[napi])
src/extractor_pdf.rs     PDF  → páginas rasterizadas (pdfium-render + image)
src/extractor_docx.rs    DOCX → modelo de documento  (zip + quick-xml)
src/renderizador_docx.rs modelo → documento dibujado   (image + ab_glyph)
```

| Formato | Flujo | Salida |
|---|---|---|
| `.pdf` | `ExtractorPdf` | una PNG por página |
| `.docx`/`.docm` | `ExtractorDocx` → `RenderizadorDocx` | una PNG (vista previa) |

## API pública (Node)

```ts
// index.d.ts (generado por napi build)
export declare function generarMiniatura(rutaEntrada: string, rutaSalida: string): number
export declare function generarMiniaturaPdf(rutaEntrada: string, rutaSalida: string): number
export declare function generarMiniaturaDocx(rutaEntrada: string, rutaSalida: string): number
export declare function generarZipMiniaturas(rutaEntrada: string): string
export declare function generarZipMiniaturasEn(rutaEntrada: string, rutaSalida: string): number
```

Valor de retorno (dependiente del formato):

| Función | Devuelve |
|---|---|
| `generarMiniatura` | cantidad de páginas renderizadas o bloques dibujados |
| `generarMiniaturaPdf` | cantidad de páginas renderizadas |
| `generarMiniaturaDocx` | cantidad total de bloques (párrafos + tablas, incluidos los de celdas) |
| `generarZipMiniaturas` | el ZIP de miniaturas codificado en base64 (sin escribir archivos) |
| `generarZipMiniaturasEn` | cantidad de imágenes empaquetadas; escribe el ZIP binario en `rutaSalida` |

Nomenclatura de salidas múltiples (PDF, ver `NombradorSalida::derivar`):
`mini.png` → `mini_p1.png` (páginas PDF). El DOCX escribe directamente en
`ruta_salida`.

## `src/lib.rs` — Orquestador

- **`struct GeneradorMiniaturas`**
  - `generar(ruta_entrada, ruta_salida) -> Result<u32, String>` — despacha
    según `extension(ruta_entrada)` al extractor responsable del formato.
  - `extension(ruta)` — extensión minúscula sin punto.
- **`pub(crate) struct NombradorSalida`**
  - `derivar(ruta_salida, sufijo) -> String` — inserta `_sufijo` antes de la
    extensión y fuerza `.png`: `mini.png` + `p1` → `mini_p1.png`.
- Funciones `#[napi]`: `generar_miniatura`, `generar_miniatura_pdf`,
  `generar_miniatura_docx` (todas delegan en los módulos y traducen
  `String` → `napi::Error`).

## ZIP en base64 — `lib.rs`

Dos rutas sobre las mismas miniaturas en memoria:

- `generarZipMiniaturas(ruta_entrada) -> String` — devuelve el ZIP
  codificado en base64, sin escribir archivos.
- `generarZipMiniaturasEn(ruta_entrada, ruta_salida) -> u32` — escribe el
  ZIP binario en `ruta_salida` (`std::fs::write`); devuelve la cantidad de
  imágenes empaquetadas.

Ambas comparten `GeneradorMiniaturas::imagenes`, que genera todas las
miniaturas en memoria.

- **`struct EmpaquetadorZip`**
  - `a_png(imagen: &DynamicImage)` — codifica una imagen a bytes PNG.
  - `bytes(imagenes: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, String>`
    — construye el ZIP en memoria con `zip::ZipWriter` sobre un `Cursor`
    (`SimpleFileOptions::default()`) y devuelve sus bytes crudos.
  - `empaquetar(imagenes) -> Result<String, String>` — `bytes` +
    `base64::prelude::BASE64_STANDARD.encode`.
- **`GeneradorMiniaturas::imagenes(ruta_entrada)`** — variante en memoria de
  `generar`; devuelve `Vec<(nombre, bytes_png)>`. Los nombres derivan del
  *stem* del archivo de entrada (`NombradorSalida::base`):
  - PDF: `<base>_pN.png` (vía `ExtractorPdf::paginas`)
  - DOCX: `<base>_pN.png` (vía `RenderizadorDocx::pngs`, en paralelo con `rayon`)

Para soportar la salida en memoria, los renderizadores exponen variantes que
comparten el código de dibujo (DRY):

- `ExtractorPdf::paginas(ruta) -> Vec<DynamicImage>` — rasteriza sin
  guardar; `renderizar_paginas` lo reutiliza.
- `RenderizadorDocx::pngs(documento) -> Vec<Vec<u8>>` — pagina y dibuja
  sobre el `Lienzo` en paralelo; `renderizar` lo reutiliza.

## PDF — `extractor_pdf.rs`

- **`static PDFIUM: OnceLock<Result<Pdfium, String>>`** — instancia única de
  pdfium por proceso: la librería nativa solo se inicializa una vez
  (`PdfiumLibraryBindingsAlreadyInitialized`); `Pdfium` es `Send + Sync`.
- **`struct ExtractorPdf`**
  - `instancia() -> Result<&'static Pdfium, String>` — carga perezosa de
    `pdfium.dll` vía `Pdfium::bind_to_system_library()`, recordando el error.
  - `renderizar_paginas(ruta, ruta_salida) -> Result<u32, String>` — itera
    todas las páginas; cada una se guarda como `<base>_p<N>.png` (via
    `NombradorSalida::derivar`) devolviendo la cantidad.
  - `renderizar_pagina(pagina) -> Result<DynamicImage, String>` — rasteriza
    una página con alto proporcional a su tamaño real (ancho fijo 1024) y la
    convierte con `bitmap.as_image()`.

### Requisito runtime

`pdfium.dll` (Windows) / `libpdfium.so` (Linux) en la raíz del proyecto o en
el `PATH`. La feature de pdfium-render debe coincidir con la build de la
DLL: se compila con `pdfium_7881` + `image_025` + `thread_safe`
(`default-features = false`) porque la DLL distribuida es la build 8046.

## Word — `extractor_docx.rs`

- **Modelo** (pub):
  - `DocumentoDocx { bloques: Vec<Bloque> }`
  - `Bloque::Parrafo(Parrafo) | Bloque::Tabla(Tabla) | Bloque::SaltoPagina`
  - `Parrafo { texto, nivel_titulo, vineta, nivel_vineta, sangria, salto_pagina, salto_antes }`
  - `Tabla { columnas, filas: Vec<Fila> }`
  - `Fila { celdas: Vec<Celda>, es_encabezado: bool }`
  - `Celda { bloques, extension: u32, fusion_v: bool, continua: bool, sombreado: Option<[u8;3]> }`
- **`struct ExtractorDocx`**
  - `extraer(ruta) -> Result<DocumentoDocx, String>` — abre el ZIP, lee
    `word/document.xml`, llama a `parsear`.
  - `parsear(xml) -> Result<DocumentoDocx, String>` — parseo streaming de
    quick-xml con **pila de contenedores** (`Contenedor::Celda|Fila|Tabla`):
    `w:tbl` apila una tabla, `w:tr` una fila, `w:tc` una celda; al cerrarse,
    cada contenido se adjunta al contenedor superior → las tablas anidadas
    funcionan por construcción. Escucha:
    - `w:p`/`w:pPr` (`pStyle` → `nivel_de_estilo`; `numPr`/`ilvl`/`ind` →
      viñeta y sangría), `w:t` (texto desescapado), `w:tab` → espacio,
      `w:br`/`w:cr` sin `type` → `\n`, `w:br type="page"` y
      `w:lastRenderedPageBreak` → `salto_pagina`, `w:pageBreakBefore` →
      `salto_antes` (al cerrar el párrafo se emite `SaltoPagina`).
    - `w:trPr/w:tblHeader` → `Fila::es_encabezado`.
    - `w:tcPr`: `w:gridSpan w:val` → `Celda::extension`;
      `w:vMerge` (`restart` → `fusion_v`, resto → `continua`);
      `w:shd w:fill` → `color_hex` → `Celda::sombreado`.
    - `w:gridCol` → cuenta columnas de `w:tblGrid`.
  - `atributo(e, nombre)` — lee un atributo por nombre local (`w:val` → `val`).
  - `color_hex(valor)` — `RRGGBB` → `[u8;3]` (`None` si `auto` o inválido).
  - `nivel_de_estilo(valor)` — `Heading2` → `2`.
  - `celda_actual_mut(pila)` — la celda más interna de la pila.

## Word — `renderizador_docx.rs`

Renderiza el modelo a PNGs de **ancho fijo 1024** y **alto fijo 1430**
(ratio A4). Layout en **dos pasadas**: medición de alturas → dibujo;
las páginas se dibujan **en paralelo** con `rayon`. La paginación respeta
saltos explícitos (`SaltoPagina`) y desborda a la página siguiente por
alto acumulado.

- **`struct EstiloTexto<'a>`** — fuente + tamaño.
  - `escala()` — `PxScaleFont` para medir/rasterizar.
  - `alto_linea()` — tamaño × `INTERLINEADO` (1.35).
  - `ancho(texto)` — suma de avances de glifo (sin kerning).
  - `dividir_lineas(texto, ancho_maximo)` — ajuste por palabras; palabras más
    anchas que la línea se cortan por caracteres; respeta `\n`.
- **`struct Lienzo`** — primitivas con recorte seguro:
  - `pixel(x, y, color, a)` — mezcla alfa acotada.
  - `rectangulo_lleno(...)` — relleno sólido (sombreado de celdas).
  - `rectangulo(...)` — contorno de 1 px (bordes de tabla).
- **`struct RenderizadorDocx { fuente: FontArc }`**
  - `nueva() -> Result<Self, String>` — carga la fuente TTF del sistema
    (caché `OnceLock`; candidatas: arial → segoeui → calibri → times →
    DejaVuSans).
  - `renderizar(ruta_salida, documento) -> Result<u32, String>` — pagina,
    dibuja en paralelo y guarda `<base>_pN.png`; devuelve nº de páginas.
  - `pngs(documento) -> Vec<Vec<u8>>` — mismo flujo en memoria (rayon).
  - `paginar(bloques, ancho) -> Vec<Vec<usize>>` — índices por página.
  - `dibujar_pagina` + `dibujar_bloque` — lienzo de alto fijo por página.
  - Medición: `alto_de_bloque`, `alto_de_bloques_en`, `alto_de_parrafo`,
    `alto_de_tabla`,
    `distribuir_tabla` (asigna columna inicial por celda respetando
    `gridSpan`; resuelve `vMerge` con un mapa columna → celda de reinicio;
    `union_v` = filas que abarca la fusión; alto de fila = máximo de
    contenidos + relleno).
  - Dibujo: `dibujar_bloques`, `dibujar_parrafo` (con `marcador_vineta`
    `•`/`◦`/`–` por nivel, `sangria_parrafo` = `w:ind` + 18 px por nivel +
    ancho del marcador), `dibujar_linea` (rasterizado glifo a glifo con
    mezcla alfa), `dibujar_tabla` (sombreado propio de la celda o
    `COLOR_ENCABEZADO` si la fila es encabezado; el borde de una celda con
    fusión vertical abarca todas sus filas; las celdas de continuación no se
    dibujan).
- `tamano_de_nivel(nivel)` — Heading1→32 px, …, cuerpo→16 px.

## Dependencias (Cargo.toml)

| Crate | Uso |
|---|---|
| `napi` / `napi-derive` / `napi-build` | frontera Node ↔ Rust |
| `pdfium-render 0.9` (features `pdfium_7881`, `image_025`, `thread_safe`) | rasterizado de PDF |
| `zip 9` | contenedor OOXML del `.docx` |
| `quick-xml 0.42` | parseo streaming de `word/document.xml` |
| `ab_glyph 0.2` | medición/rasterizado de texto |
| `rayon 1` | paralelismo al dibujar páginas DOCX |
| `zip 9` (escritura) / `base64 0.23` | ZIP en memoria + base64 |
| `image 0.25` | lienzo RGBA y codificación PNG |

## Cómo se construye y prueba

```bash
yarn build:debug        # compila el .node y regenera index.d.ts / index.js
yarn build              # release
```

```js
const { generarMiniatura, generarZipMiniaturas } = require('extractor_miniaturas');
generarMiniatura('doc.pdf',    'mini.png'); // → mini_p1.png, mini_p2.png, …
generarMiniatura('doc.docx',   'mini.png'); // → mini_p1.png, mini_p2.png, … (paginado)

const zipBase64 = generarZipMiniaturas('doc.pdf'); // → "UEsDBBQAA…" (ZIP en base64)
generarZipMiniaturasEn('doc.pdf', 'salida.zip');   // → escribe salida.zip (N)
```

Errores típicos (lanzados como excepción Node con mensaje descriptivo):
archivo inexistente, formato no soportado, ZIP/XML inválido,
`pdfium.dll` ausente o de versión incompatible, fuente TTF del sistema no
encontrada.
