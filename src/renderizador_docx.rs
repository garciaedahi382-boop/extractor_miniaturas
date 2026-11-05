//! Renderizador de documentos DOCX a imágenes PNG, implementado sobre
//! [image] (lienzo y codificación PNG) y [ab_glyph] (medición y rasterizado
//! de fuentes). El documento se pagina por saltos explícitos (`SaltoPagina`)
//! y por alto fijo (`ALTO_PAGINA`, ratio A4 para 1024 px), y cada página se
//! dibuja **en paralelo** con `rayon`.
//!
//! El layout de cada página se hace en dos pasadas: primero se calculan
//! las alturas de sus bloques (incluidas tablas anidadas) y luego se
//! dibuja. Las celdas combinadas horizontalmente (`gridSpan`) ocupan
//! varias columnas y las fusiones verticales (`vMerge`) dibujan un solo
//! borde que abarca todas sus filas; las celdas de continuación no se
//! dibujan.

use std::collections::HashMap;
use std::sync::OnceLock;

use ab_glyph::{Font, FontArc, PxScale, PxScaleFont, ScaleFont};
use image::RgbaImage;

/// Ancho fijo en píxeles del lienzo de la miniatura.
const ANCHO: u32 = 1024;
/// Márgenes exteriores del documento.
const MARGEN: u32 = 48;
/// Alto fijo de cada página (ratio A4 para 1024 px: 1024·√2 ≈ 1448).
const ALTO_PAGINA: u32 = 1430;
/// Relleno interior de las celdas de tabla.
const RELLENO_CELDA: f32 = 8.0;
/// Espacio vertical entre bloques (párrafos/tablas).
const ESPACIO_BLOQUE: f32 = 12.0;
/// Interlineado como proporción del tamaño de fuente.
const INTERLINEADO: f32 = 1.35;
/// Alto mínimo de fila de tabla (celdas vacías o de solo continuación).
const ALTO_MINIMO_FILA: f32 = 26.0;

const COLOR_FONDO: [u8; 3] = [255, 255, 255];
const COLOR_TEXTO: [u8; 3] = [40, 40, 40];
const COLOR_BORDE: [u8; 3] = [150, 150, 150];
const COLOR_ENCABEZADO: [u8; 3] = [233, 236, 239];

/// Rutas de fuentes de sistema candidatas (primera disponible gana).
const FUENTES_CANDIDATAS: &[&str] = &[
  "C:/Windows/Fonts/arial.ttf",
  "C:/Windows/Fonts/segoeui.ttf",
  "C:/Windows/Fonts/calibri.ttf",
  "C:/Windows/Fonts/times.ttf",
  "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
  "/usr/share/fonts/dejavu/DejaVuSans.ttf",
];

use crate::extractor_docx::{Bloque, Celda, DocumentoDocx, Parrafo, Tabla};
use rayon::prelude::*;

/// Tamaño de fuente según el nivel de título (`Heading1..6`).
fn tamano_de_nivel(nivel: Option<u8>) -> f32 {
  match nivel {
    Some(1) => 32.0,
    Some(2) => 26.0,
    Some(3) => 22.0,
    Some(4) => 20.0,
    Some(5) => 18.0,
    Some(6) => 17.0,
    _ => 16.0,
  }
}

/// Fuente del sistema cargada perezosamente y compartida por el proceso.
static FUENTE: OnceLock<Result<FontArc, String>> = OnceLock::new();

/// Estilo tipográfico: fuente + tamaño en píxeles.
struct EstiloTexto<'a> {
  fuente: &'a FontArc,
  tamano: f32,
}

impl<'a> EstiloTexto<'a> {
  fn nueva(fuente: &'a FontArc, tamano: f32) -> Self {
    EstiloTexto { fuente, tamano }
  }

  /// Escala tipográfica en píxeles para medir y rasterizar.
  fn escala(&self) -> PxScaleFont<&'a FontArc> {
    self.fuente.as_scaled(PxScale::from(self.tamano))
  }

  /// Alto de línea en píxeles.
  fn alto_linea(&self) -> f32 {
    self.tamano * INTERLINEADO
  }

  /// Ancho en píxeles de un texto (sin kerning, suficiente para layout).
  fn ancho(&self, texto: &str) -> f32 {
    let escala = self.escala();
    texto
      .chars()
      .map(|c| escala.h_advance(escala.glyph_id(c)))
      .sum()
  }

  /// Ajuste de línea por palabras; las palabras más anchas que la línea se
  /// cortan por caracteres. Respeta saltos explícitos (`\n`).
  fn dividir_lineas(&self, texto: &str, ancho_maximo: f32) -> Vec<String> {
    let mut lineas = Vec::new();
    for segmento in texto.split('\n') {
      let mut actual = String::new();
      for palabra in segmento.split_whitespace() {
        if self.ancho(palabra) > ancho_maximo {
          if !actual.is_empty() {
            lineas.push(std::mem::take(&mut actual));
          }
          for c in palabra.chars() {
            actual.push(c);
            if self.ancho(&actual) > ancho_maximo {
              let ultimo = actual.pop().unwrap_or(' ');
              if !actual.is_empty() {
                lineas.push(std::mem::take(&mut actual));
              }
              actual.push(ultimo);
            }
          }
          continue;
        }
        let candidata = if actual.is_empty() {
          palabra.to_string()
        } else {
          format!("{actual} {palabra}")
        };
        if self.ancho(&candidata) > ancho_maximo {
          lineas.push(std::mem::take(&mut actual));
        }
        actual = if actual.is_empty() {
          palabra.to_string()
        } else {
          format!("{actual} {palabra}")
        };
      }
      lineas.push(actual);
    }
    lineas
  }
}

/// Lienzo con primitivas de dibujo acotadas (todo recorte es seguro).
struct Lienzo {
  img: RgbaImage,
}

impl Lienzo {
  fn nuevo(ancho: u32, alto: u32) -> Self {
    Lienzo { img: RgbaImage::from_pixel(ancho, alto, image::Rgba([COLOR_FONDO[0], COLOR_FONDO[1], COLOR_FONDO[2], 255])) }
  }

  fn alto(&self) -> f32 {
    self.img.height() as f32
  }

  /// Mezcla un color sobre el pixel con cobertura `a` (0..1).
  fn pixel(&mut self, x: i32, y: i32, color: [u8; 3], a: f32) {
    if x < 0 || y < 0 {
      return;
    }
    let (x, y) = (x as u32, y as u32);
    if x >= self.img.width() || y >= self.img.height() {
      return;
    }
    let a = a.clamp(0.0, 1.0);
    let previo = self.img.get_pixel(x, y).0;
    let mezcla = |fondo: u8, frente: u8| -> u8 {
      (fondo as f32 * (1.0 - a) + frente as f32 * a).round() as u8
    };
    self.img.put_pixel(
      x,
      y,
      image::Rgba([
        mezcla(previo[0], color[0]),
        mezcla(previo[1], color[1]),
        mezcla(previo[2], color[2]),
        255,
      ]),
    );
  }

  /// Relleno sólido de un rectángulo (para sombreado de celdas).
  fn rectangulo_lleno(&mut self, x: f32, y: f32, ancho: f32, alto: f32, color: [u8; 3]) {
    let x0 = x.floor().max(0.0) as i32;
    let y0 = y.floor().max(0.0) as i32;
    let x1 = ((x + ancho).ceil() as i32).min(self.img.width() as i32);
    let y1 = ((y + alto).ceil() as i32).min(self.img.height() as i32);
    for py in y0..y1 {
      for px in x0..x1 {
        self.pixel(px, py, color, 1.0);
      }
    }
  }

  /// Contorno de un rectángulo (1 px) con coordenadas fraccionarias.
  fn rectangulo(&mut self, x: f32, y: f32, ancho: f32, alto: f32, color: [u8; 3]) {
    if ancho <= 0.0 || alto <= 0.0 {
      return;
    }
    let (x0, y0, x1, y1) = (x.round() as i32, y.round() as i32, (x + ancho).round() as i32, (y + alto).round() as i32);
    for px in x0..=x1 {
      self.pixel(px, y0, color, 1.0);
      self.pixel(px, y1, color, 1.0);
    }
    for py in y0..=y1 {
      self.pixel(x0, py, color, 1.0);
      self.pixel(x1, py, color, 1.0);
    }
  }
}

/// Posición calculada de una celda en la grilla de su tabla.
struct CeldaPosicion<'a> {
  celda: &'a Celda,
  /// Columna inicial (base 0).
  inicio: usize,
  /// Columnas ocupadas.
  extension: usize,
  /// Filas que abarca (1 = normal; >1 por fusión vertical).
  union_v: usize,
  /// Es celda de continuación de una fusión vertical (no se dibuja).
  continua: bool,
}

/// Posición calculada de una fila.
struct FilaPosicion<'a> {
  alto: f32,
  celdas: Vec<CeldaPosicion<'a>>,
  /// La fila es un encabezado de tabla (para sombreado por defecto).
  es_encabezado: bool,
}

/// Layout completo de una tabla.
struct TablaPosicion<'a> {
  ancho_columna: f32,
  filas: Vec<FilaPosicion<'a>>,
  alto_total: f32,
}

/// Renderiza un `DocumentoDocx` a una imagen PNG.
pub struct RenderizadorDocx {
  fuente: FontArc,
}

impl RenderizadorDocx {
  /// Carga la fuente del sistema (caché de proceso); la primera candidata
  /// disponible gana.
  pub fn nueva() -> Result<Self, String> {
    let fuente = FUENTE
      .get_or_init(|| {
        for ruta in FUENTES_CANDIDATAS {
          if let Ok(bytes) = std::fs::read(ruta) {
            if let Ok(fuente) = FontArc::try_from_vec(bytes) {
              return Ok(fuente);
            }
          }
        }
        Err(format!(
          "no se encontró una fuente TTF del sistema (candidatas: {})",
          FUENTES_CANDIDATAS.join(", ")
        ))
      })
      .as_ref()
      .map_err(|e| e.clone())?;
    Ok(RenderizadorDocx { fuente: fuente.clone() })
  }

  /// Renderiza el documento paginado y guarda cada página como...
  /// Usa `NombradorSalida::derivar` vía `crate` (compartido con `lib.rs`).
  /// Devuelve la cantidad de páginas generadas.
  pub fn renderizar(&self, ruta_salida: &str, documento: &DocumentoDocx) -> Result<u32, String> {
    let paginas = self.pngs(documento)?;
    for (i, bytes) in paginas.iter().enumerate() {
      let salida = crate::NombradorSalida::derivar(ruta_salida, &format!("p{}", i + 1));
      std::fs::write(&salida, bytes)
        .map_err(|e| format!("error al escribir el PNG: {e}"))?;
    }
    Ok(paginas.len() as u32)
  }

  /// Renderiza el documento paginado a bytes PNG en memoria (una entrada
  /// por página), sin tocar el sistema de archivos. Las páginas se dibujan
  /// en paralelo con `rayon`.
  pub fn pngs(&self, documento: &DocumentoDocx) -> Result<Vec<Vec<u8>>, String> {
    let ancho_util = (ANCHO - 2 * MARGEN) as f32;
    let paginas = self.paginar(&documento.bloques, ancho_util);
    paginas
      .par_iter()
      .map(|indices| self.dibujar_pagina(&documento.bloques, indices, ancho_util))
      .collect()
  }

  /// Reparte los bloques en páginas por saltos explícitos (`SaltoPagina`)
  /// y por alto fijo (`ALTO_PAGINA`). Devuelve, por página, los índices
  /// de los bloques a dibujar en ella.
  fn paginar(&self, bloques: &[Bloque], ancho_util: f32) -> Vec<Vec<usize>> {
    let mut paginas: Vec<Vec<usize>> = Vec::new();
    let mut actual: Vec<usize> = Vec::new();
    let mut alto_acumulado = MARGEN as f32;
    let limite = ALTO_PAGINA as f32 - MARGEN as f32;
    for (i, bloque) in bloques.iter().enumerate() {
      if matches!(bloque, Bloque::SaltoPagina) {
        if !actual.is_empty() || !paginas.is_empty() {
          paginas.push(std::mem::take(&mut actual));
          alto_acumulado = MARGEN as f32;
        }
        continue;
      }
      let alto = self.alto_de_bloque(bloque, ancho_util);
      if !actual.is_empty() && alto_acumulado + alto > limite {
        paginas.push(std::mem::take(&mut actual));
        alto_acumulado = MARGEN as f32;
      }
      actual.push(i);
      alto_acumulado += alto;
    }
    // La última página puede estar vacía tras un salto final: no emitirla.
    if !actual.is_empty() || paginas.is_empty() {
      paginas.push(actual);
    }
    paginas
  }

  /// Dibuja una página sobre un lienzo de alto fijo y la codifica a PNG.
  fn dibujar_pagina(
    &self,
    bloques: &[Bloque],
    indices: &[usize],
    ancho_util: f32,
  ) -> Result<Vec<u8>, String> {
    let mut lienzo = Lienzo::nuevo(ANCHO, ALTO_PAGINA);
    let mut y = MARGEN as f32;
    for &i in indices {
      match &bloques[i] {
        Bloque::Parrafo(_) | Bloque::Tabla(_) => {
          y = self.dibujar_bloque(&mut lienzo, &bloques[i], MARGEN as f32, y, ancho_util);
          y += ESPACIO_BLOQUE;
        }
        Bloque::SaltoPagina => {}
      }
      if y >= lienzo.alto() {
        break;
      }
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    lienzo
      .img
      .write_to(&mut cursor, image::ImageFormat::Png)
      .map_err(|e| format!("error al codificar el PNG del documento: {e}"))?;
    Ok(cursor.into_inner())
  }

  /// Dibuja un bloque en `(x, y)` y devuelve la `y` tras él (sin bloque).
  fn dibujar_bloque(
    &self,
    lienzo: &mut Lienzo,
    bloque: &Bloque,
    x: f32,
    y: f32,
    ancho: f32,
  ) -> f32 {
    match bloque {
      Bloque::Parrafo(p) => self.dibujar_parrafo(lienzo, p, x, y, ancho),
      Bloque::Tabla(t) => self.dibujar_tabla(lienzo, t, x, y, ancho),
      Bloque::SaltoPagina => y,
    }
  }

  /// Alto de un bloque (medición de la primera pasada).
  fn alto_de_bloque(&self, bloque: &Bloque, ancho: f32) -> f32 {
    match bloque {
      Bloque::Parrafo(p) => self.alto_de_parrafo(p, ancho),
      Bloque::Tabla(t) => self.alto_de_tabla(t, ancho),
      Bloque::SaltoPagina => 0.0,
    }
  }

  /// Alto conjunto de bloques (saltos de página incluidos, cuentan 0).
  fn alto_de_bloques_en(&self, bloques: &[Bloque], ancho: f32) -> f32 {
    bloques.iter().map(|b| self.alto_de_bloque(b, ancho)).sum()
  }

  // ----- Medición de alturas (primera pasada) -----

  /// Marcador de viñeta según el nivel (`w:numPr`):
  /// 0 → `•`, 1 → `◦`, 2+ → `–`.
  fn marcador_vineta(nivel: u32) -> &'static str {
    match nivel {
      0 => "\u{2022}", // bullet
      1 => "\u{25CB}", // white circle
      _ => "\u{2013}",  // en dash
    }
  }

  /// Sangría total a la izquierda: la propia del párrafo (`w:ind`), el
  /// nivel de viñeta y el ancho del marcador con su espaciado.
  fn sangria_parrafo(&self, estilo: &EstiloTexto<'_>, p: &Parrafo) -> f32 {
    if p.vineta {
      let marcador = EstiloTexto::nueva(&self.fuente, estilo.tamano);
      p.sangria
        + (p.nivel_vineta as f32) * 18.0
        + marcador.ancho(RenderizadorDocx::marcador_vineta(p.nivel_vineta))
        + 6.0
    } else {
      p.sangria
    }
  }

  fn alto_de_parrafo(&self, p: &Parrafo, ancho: f32) -> f32 {
    let estilo = EstiloTexto::nueva(&self.fuente, tamano_de_nivel(p.nivel_titulo));
    let sangria = self.sangria_parrafo(&estilo, p);
    let lineas = estilo.dividir_lineas(&p.texto, (ancho - sangria).max(1.0));
    (lineas.len() as f32) * estilo.alto_linea() + ESPACIO_BLOQUE
  }

  fn alto_de_tabla(&self, tabla: &Tabla, ancho: f32) -> f32 {
    self.distribuir_tabla(tabla, ancho).alto_total
  }

  /// Calcula el layout de la tabla: asigna columnas de inicio a cada celda
  /// respetando `gridSpan` y `vMerge`, y mide la altura de cada fila como
  /// el máximo de las alturas de contenido de sus celdas.
  fn distribuir_tabla<'a>(&self, tabla: &'a Tabla, ancho: f32) -> TablaPosicion<'a> {
    let suma_maxima = tabla
      .filas
      .iter()
      .map(|f| f.celdas.iter().map(|c| c.extension).sum::<u32>())
      .max()
      .unwrap_or(1) as usize;
    let columnas = tabla.columnas.max(suma_maxima).max(1);
    let ancho_columna = ancho / columnas as f32;

    // Columna → (fila, índice) de la celda que inició una fusión vertical.
    let mut fusiones: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut filas: Vec<FilaPosicion<'a>> = Vec::new();

    for fila in &tabla.filas {
      let mut col = 0usize;
      let mut celdas: Vec<CeldaPosicion<'a>> = Vec::new();
      for celda in &fila.celdas {
        if celda.continua {
          // La celda de continuación extiende la fusión iniciada arriba.
          if let Some(&(f, i)) = fusiones.get(&col) {
            filas[f].celdas[i].union_v += 1;
          }
        } else {
          // Si la fusión anterior terminó, liberar la columna.
          fusiones.remove(&col);
          let indice = celdas.len();
          celdas.push(CeldaPosicion {
            celda,
            inicio: col,
            extension: (celda.extension.max(1)) as usize,
            union_v: 1,
            continua: false,
          });
          if celda.fusion_v {
            fusiones.insert(col, (filas.len(), indice));
          }
        }
        col += (celda.extension.max(1)) as usize;
      }

      let mut alto = ALTO_MINIMO_FILA;
      for c in &celdas {
        let ancho_celda = ancho_columna * c.extension as f32 - 2.0 * RELLENO_CELDA;
        if ancho_celda > 0.0 {
          alto = alto.max(
            self.alto_de_bloques_en(&c.celda.bloques, ancho_celda) + 2.0 * RELLENO_CELDA,
          );
        }
      }
      filas.push(FilaPosicion { alto, celdas, es_encabezado: fila.es_encabezado });
    }

    let alto_total: f32 = filas.iter().map(|f| f.alto).sum();
    TablaPosicion { ancho_columna, filas, alto_total }
  }

  // ----- Dibujo (segunda pasada) -----

  fn dibujar_bloques(
    &self,
    lienzo: &mut Lienzo,
    bloques: &[Bloque],
    x: f32,
    y: f32,
    ancho: f32,
  ) -> f32 {
    let mut y = y;
    for bloque in bloques {
      y = match bloque {
        Bloque::Parrafo(p) => self.dibujar_parrafo(lienzo, p, x, y, ancho) + ESPACIO_BLOQUE,
        Bloque::Tabla(t) => self.dibujar_tabla(lienzo, t, x, y, ancho) + ESPACIO_BLOQUE,
        Bloque::SaltoPagina => y,
      };
      if y >= lienzo.alto() {
        break;
      }
    }
    y
  }

  fn dibujar_parrafo(
    &self,
    lienzo: &mut Lienzo,
    p: &Parrafo,
    x: f32,
    y: f32,
    ancho: f32,
  ) -> f32 {
    let estilo = EstiloTexto::nueva(&self.fuente, tamano_de_nivel(p.nivel_titulo));
    let sangria = self.sangria_parrafo(&estilo, p);
    let lineas = estilo.dividir_lineas(&p.texto, (ancho - sangria).max(1.0));

    // Marcador de viñeta en la primera línea, en la sangría base.
    if p.vineta {
      let marcador = EstiloTexto::nueva(&self.fuente, estilo.tamano);
      let posicion = ab_glyph::point(
        x + p.sangria + (p.nivel_vineta as f32) * 18.0,
        y + marcador.escala().ascent(),
      );
      let mut glifo = marcador.escala().scaled_glyph(
        RenderizadorDocx::marcador_vineta(p.nivel_vineta).chars().next().unwrap_or('\u{2022}'),
      );
      glifo.position = posicion;
      if let Some(contorno) = marcador.escala().font().outline_glyph(glifo) {
        let limites = contorno.px_bounds();
        contorno.draw(|gx, gy, cobertura| {
          let px = (limites.min.x + gx as f32).round() as i32;
          let py = (limites.min.y + gy as f32).round() as i32;
          if cobertura > 0.0 {
            lienzo.pixel(px, py, COLOR_TEXTO, cobertura);
          }
        });
      }
    }

    let mut baseline = y + estilo.escala().ascent();
    for linea in &lineas {
      if baseline >= lienzo.alto() {
        break;
      }
      self.dibujar_linea(lienzo, &estilo, linea, x + sangria, baseline);
      baseline += estilo.alto_linea();
    }
    y + (lineas.len() as f32) * estilo.alto_linea()
  }

  /// Dibujo de una línea de texto: por cada carácter se rasteriza su
  /// contorno y se mezcla la cobertura sobre el lienzo.
  fn dibujar_linea(
    &self,
    lienzo: &mut Lienzo,
    estilo: &EstiloTexto<'_>,
    linea: &str,
    x: f32,
    baseline: f32,
  ) {
    let escala = estilo.escala();
    let mut cursor = x;
    for c in linea.chars() {
      let mut glifo = escala.scaled_glyph(c);
      glifo.position = ab_glyph::point(cursor, baseline);
      let avance = escala.h_advance(escala.glyph_id(c));
      if let Some(contorno) = escala.font().outline_glyph(glifo) {
        let limites = contorno.px_bounds();
        contorno.draw(|gx, gy, cobertura| {
          let px = (limites.min.x + gx as f32).round() as i32;
          let py = (limites.min.y + gy as f32).round() as i32;
          if cobertura > 0.0 {
            lienzo.pixel(px, py, COLOR_TEXTO, cobertura);
          }
        });
      }
      cursor += avance;
    }
  }

  fn dibujar_tabla(
    &self,
    lienzo: &mut Lienzo,
    tabla: &Tabla,
    x: f32,
    y: f32,
    ancho: f32,
  ) -> f32 {
    let pos = self.distribuir_tabla(tabla, ancho);

    // Alturas acumuladas para calcular el alto de celdas con fusión vertical.
    let mut acumuladas: Vec<f32> = Vec::with_capacity(pos.filas.len() + 1);
    acumuladas.push(y);
    for fila in &pos.filas {
      let ultima = *acumuladas.last().unwrap_or(&y);
      acumuladas.push(ultima + fila.alto);
    }

    let mut y_fila = y;
    for (i, fila) in pos.filas.iter().enumerate() {
      if y_fila >= lienzo.alto() {
        break;
      }
      for celda in &fila.celdas {
        if celda.continua {
          continue;
        }
        let cx = x + celda.inicio as f32 * pos.ancho_columna;
        let ancho_celda = pos.ancho_columna * celda.extension as f32;
        if ancho_celda <= 0.0 {
          continue;
        }
        // Alto total de la celda (incluye filas unidas verticalmente).
        let fin = (i + celda.union_v).min(pos.filas.len());
        let alto = acumuladas[fin] - acumuladas[i];
        // Sombreado: el propio de la celda o, si es fila de encabezado,
        // un gris por defecto. No se dibuja si coincide con el fondo.
        let fondo = celda.celda.sombreado.unwrap_or(if fila.es_encabezado {
          COLOR_ENCABEZADO
        } else {
          COLOR_FONDO
        });
        if fondo != COLOR_FONDO {
          lienzo.rectangulo_lleno(cx, y_fila, ancho_celda, alto, fondo);
        }
        // Contenido de la celda (puede incluir tablas anidadas).
        self.dibujar_bloques(
          lienzo,
          &celda.celda.bloques,
          cx + RELLENO_CELDA,
          y_fila + RELLENO_CELDA,
          ancho_celda - 2.0 * RELLENO_CELDA,
        );
        // Borde que abarca todas las filas de la fusión vertical.
        lienzo.rectangulo(cx, y_fila, ancho_celda, alto, COLOR_BORDE);
      }
      y_fila += fila.alto;
    }

    y + pos.alto_total
  }
}
