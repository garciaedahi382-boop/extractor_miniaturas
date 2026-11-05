#![deny(clippy::all)]

//! `extractor_miniaturas` — Addon de Node (napi-rs) que genera miniaturas
//! PNG a partir de distintos formatos de entrada, separando
//! responsabilidades por módulo:
//!
//! | Módulo | Responsabilidad |
//! |---|---|
//! | `extractor_pdf` | Rasteriza páginas PDF con [pdfium-render] y guarda con [image] |
//! | `extractor_docx` | Parsea cuerpo y tablas de `.docx` con [zip] + [quick_xml] |
//! | `renderizador_docx` | Dibuja el documento a PNG con [image] + [ab_glyph] |
//! | `GeneradorMiniaturas` | Orquestador: despacha por extensión del archivo |

mod extractor_docx;
mod extractor_pdf;
mod renderizador_docx;

use base64::Engine;
use extractor_docx::ExtractorDocx;
use extractor_pdf::ExtractorPdf;
use image::DynamicImage;
use napi_derive::napi;
use renderizador_docx::RenderizadorDocx;

/// Deriva rutas de salida múltiples a partir de una ruta base, insertando
/// un sufijo numerado antes de la extensión: `mini.png` con sufijo `p1`
/// produce `mini_p1.png`. Compartido por los extractores que generan una
/// salida por unidad (página de PDF).
pub(crate) struct NombradorSalida;

impl NombradorSalida {
  /// Nombre base (sin extensión) de una ruta, usado para nombrar las
  /// entradas dentro del ZIP.
  pub(crate) fn base(ruta: &str) -> String {
    std::path::Path::new(ruta)
      .file_stem()
      .map(|s| s.to_string_lossy().into_owned())
      .unwrap_or_else(|| ruta.to_string())
  }

  /// Inserta `sufijo` (p. ej. `"p1"`, `"p2"`) en `ruta_salida` antes de la
  /// extensión, forzando la extensión `.png`.
  pub(crate) fn derivar(ruta_salida: &str, sufijo: &str) -> String {
    let ruta = std::path::Path::new(ruta_salida);
    let carpeta = ruta.parent().filter(|c| !c.as_os_str().is_empty());
    let base = ruta
      .file_stem()
      .map(|s| s.to_string_lossy().into_owned())
      .unwrap_or_else(|| ruta_salida.to_string());
    let nombre = format!("{}_{}.png", base, sufijo);
    match carpeta {
      Some(c) => c.join(nombre).to_string_lossy().into_owned(),
      None => nombre,
    }
  }
}

/// Orquestador de miniaturas: despacha según la extensión del archivo
/// de entrada al extractor responsable de ese formato.
struct GeneradorMiniaturas;

impl GeneradorMiniaturas {
  /// Genera las miniaturas PNG para el archivo `ruta_entrada`, una por
  /// unidad (página de PDF/Word). Devuelve un valor dependiente del formato:
  /// - `.pdf`: cantidad de páginas renderizadas (todas, una PNG por página).
  /// - `.docx`/`.docm`: cantidad de páginas generadas (paginado por saltos
  ///   explícitos y alto fijo, en paralelo).
  fn generar(ruta_entrada: &str, ruta_salida: &str) -> Result<u32, String> {
    match GeneradorMiniaturas::extension(ruta_entrada).as_str() {
      "pdf" => ExtractorPdf::renderizar_paginas(ruta_entrada, ruta_salida),
      "docx" | "docm" => {
        let documento = ExtractorDocx::extraer(ruta_entrada)?;
        RenderizadorDocx::nueva()?.renderizar(ruta_salida, &documento)
      }
      extension => Err(format!("formato '{extension}' no soportado")),
    }
  }

  /// Genera las miniaturas PNG **en memoria** para `ruta_entrada`, sin
  /// escribir archivos. Devuelve pares (nombre dentro del ZIP, bytes PNG).
  fn imagenes(ruta_entrada: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let base = NombradorSalida::base(ruta_entrada);
    match GeneradorMiniaturas::extension(ruta_entrada).as_str() {
      "pdf" => {
        let paginas = ExtractorPdf::paginas(ruta_entrada)?;
        paginas
          .iter()
          .enumerate()
          .map(|(indice, pagina)| {
            Ok((
              format!("{}_p{}.png", base, indice + 1),
              EmpaquetadorZip::a_png(pagina)?,
            ))
          })
          .collect()
      }
      "docx" | "docm" => {
        let documento = ExtractorDocx::extraer(ruta_entrada)?;
        let paginas = RenderizadorDocx::nueva()?.pngs(&documento)?;
        Ok(paginas
          .into_iter()
          .enumerate()
          .map(|(i, bytes)| (format!("{}_p{}.png", base, i + 1), bytes))
          .collect())
      }
      extension => Err(format!("formato '{extension}' no soportado")),
    }
  }

  /// Extrae la extensión (minúsculas, sin punto) de una ruta.
  fn extension(ruta: &str) -> String {
    std::path::Path::new(ruta)
      .extension()
      .and_then(|e| e.to_str())
      .map(|e| e.to_ascii_lowercase())
      .unwrap_or_default()
  }
}

/// Genera la miniatura PNG para cualquier formato soportado
/// (`.pdf` → una PNG por página; `.docx`/`.docm` → una PNG por página).
///
/// # Returns
/// Un valor dependiente del formato: páginas renderizadas (`.pdf`)
/// o páginas generadas (`.docx`).
///
/// # Errors
/// Si el formato no está soportado, el archivo no puede abrirse, no hay
/// datos utilizables o falla la escritura del PNG.
#[napi]
pub fn generar_miniatura(ruta_entrada: String, ruta_salida: String) -> napi::Result<u32> {
  GeneradorMiniaturas::generar(&ruta_entrada, &ruta_salida)
    .map_err(|e| napi::Error::from_reason(e))
}

/// Genera miniaturas PNG de un documento PDF: renderiza todas sus páginas.
///
/// Cada página se guarda como PNG derivando el nombre de `ruta_salida`
/// con el número de página: `mini.png` → `mini_p1.png`, `mini_p2.png`, …
///
/// # Returns
/// Cantidad de páginas renderizadas.
///
/// # Errors
/// Si el documento no puede abrirse, la librería dinámica de pdfium no
/// está disponible o falla la escritura de alguna PNG.
#[napi]
pub fn generar_miniatura_pdf(ruta_entrada: String, ruta_salida: String) -> napi::Result<u32> {
  ExtractorPdf::renderizar_paginas(&ruta_entrada, &ruta_salida)
    .map_err(napi::Error::from_reason)
}

/// Genera miniaturas PNG paginadas de un documento Word (`.docx`/`.docm`):
/// dibuja párrafos (con tamaños según `Heading1..6`) y tablas con celdas
/// combinadas (`gridSpan`), fusiones verticales (`vMerge`) y tablas
/// anidadas dentro de celdas. La paginación respeta saltos explícitos
/// (`w:br type="page"`, `lastRenderedPageBreak`, `pageBreakBefore`) y
/// alto fijo, y las páginas se dibujan en paralelo con `rayon`.
///
/// Cada página se guarda derivando `ruta_salida` con `_pN`.
///
/// # Returns
/// Cantidad de páginas generadas.
///
/// # Errors
/// Si el documento no puede abrirse, no es un `.docx` válido, no hay
/// fuente TTF del sistema disponible o falla la escritura del PNG.
#[napi]
pub fn generar_miniatura_docx(ruta_entrada: String, ruta_salida: String) -> napi::Result<u32> {
  let documento = ExtractorDocx::extraer(&ruta_entrada).map_err(napi::Error::from_reason)?;
  RenderizadorDocx::nueva()
    .and_then(|renderizador| renderizador.renderizar(&ruta_salida, &documento))
    .map_err(napi::Error::from_reason)
}

/// Empaqueta imágenes PNG en un ZIP en memoria y lo codifica en base64.
pub(crate) struct EmpaquetadorZip;

impl EmpaquetadorZip {
  /// Codifica una imagen en memoria a bytes PNG.
  pub(crate) fn a_png(imagen: &DynamicImage) -> Result<Vec<u8>, String> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    imagen
      .write_to(&mut cursor, image::ImageFormat::Png)
      .map_err(|e| format!("error al codificar el PNG: {e}"))?;
    Ok(cursor.into_inner())
  }

  /// Construye un ZIP en memoria con las entradas (nombre, bytes PNG) y
  /// devuelve sus bytes crudos.
  pub(crate) fn bytes(imagenes: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, String> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
      let mut zip = zip::ZipWriter::new(&mut cursor);
      let opciones = zip::write::SimpleFileOptions::default();
      for (nombre, bytes) in imagenes {
        zip
          .start_file(&nombre, opciones)
          .map_err(|e| format!("error al agregar '{nombre}' al ZIP: {e}"))?;
        std::io::Write::write_all(&mut zip, &bytes)
          .map_err(|e| format!("error al escribir '{nombre}' en el ZIP: {e}"))?;
      }
      zip
        .finish()
        .map_err(|e| format!("error al finalizar el ZIP: {e}"))?;
    }
    Ok(cursor.into_inner())
  }

  /// Construye el ZIP en memoria y lo devuelve codificado en base64.
  pub(crate) fn empaquetar(imagenes: Vec<(String, Vec<u8>)>) -> Result<String, String> {
    Ok(base64::prelude::BASE64_STANDARD.encode(EmpaquetadorZip::bytes(imagenes)?))
  }
}

/// Genera las miniaturas PNG del archivo `ruta_entrada` (cualquier formato
/// soportado: `.pdf`, `.docx`/`.docm`) sin escribir
/// archivos, las empaqueta en un ZIP en memoria y devuelve el ZIP
/// codificado en base64.
///
/// # Returns
/// El ZIP en base64; dentro del ZIP, una PNG por unidad con nombres
/// `<base>_p1.png` (PDF o Word).
///
/// # Errors
/// Si el formato no está soportado, el archivo no puede abrirse, no hay
/// datos utilizables o falla la construcción del ZIP.
#[napi]
pub fn generar_zip_miniaturas(ruta_entrada: String) -> napi::Result<String> {
  let imagenes = GeneradorMiniaturas::imagenes(&ruta_entrada)
    .map_err(napi::Error::from_reason)?;
  EmpaquetadorZip::empaquetar(imagenes).map_err(napi::Error::from_reason)
}

/// Genera las miniaturas PNG del archivo `ruta_entrada` (cualquier formato
/// soportado) en memoria, las empaqueta en un ZIP y **lo escribe** en
/// `ruta_salida` como archivo binario `.zip`.
///
/// # Returns
/// Cantidad de imágenes empaquetadas en el ZIP.
///
/// # Errors
/// Si el formato no está soportado, el archivo no puede abrirse, no hay
/// datos utilizables, falla la construcción del ZIP o falla la escritura
/// del archivo en `ruta_salida`.
#[napi]
pub fn generar_zip_miniaturas_en(
  ruta_entrada: String,
  ruta_salida: String,
) -> napi::Result<u32> {
  let imagenes = GeneradorMiniaturas::imagenes(&ruta_entrada)
    .map_err(napi::Error::from_reason)?;
  let cantidad = imagenes.len() as u32;
  let bytes = EmpaquetadorZip::bytes(imagenes).map_err(napi::Error::from_reason)?;
  std::fs::write(&ruta_salida, bytes).map_err(|e| {
    napi::Error::from_reason(format!("error al escribir el ZIP: {e}"))
  })?;
  Ok(cantidad)
}
