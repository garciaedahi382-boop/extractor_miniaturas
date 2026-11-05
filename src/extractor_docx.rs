//! Extractor de documentos Microsoft Word (`.docx`), implementado sobre
//! [zip] (contenedor OOXML) y [quick_xml] (parseo streaming de XML).
//!
//! El modelo soporta el contenido mixto del cuerpo: párrafos, tablas y
//! saltos de página (`w:br w:type="page"`, `w:lastRenderedPageBreak` y
//! `w:pageBreakBefore`), incluyendo celdas combinadas horizontalmente
//! (`w:gridSpan`), fusiones verticales (`w:vMerge`) y tablas anidadas.

use std::io::Read;

use quick_xml::Reader;
use quick_xml::events::Event;

/// Documento DOCX extraído: bloques de primer nivel del cuerpo.
pub struct DocumentoDocx {
  pub bloques: Vec<Bloque>,
}

/// Bloque de contenido: párrafo, tabla o salto de página.
pub enum Bloque {
  Parrafo(Parrafo),
  Tabla(Tabla),
  /// Salto de página explícito (el `Parrafo` anterior termina la página).
  SaltoPagina,
}

/// Párrafo con su texto, su nivel de título, su nivel de
/// viñeta y su sangría en píxeles y saltos de línea/página internos.
pub struct Parrafo {
  pub texto: String,
  pub nivel_titulo: Option<u8>,
  /// El párrafo es un elemento de lista con viñeta.
  pub vineta: bool,
  /// Nivel de la viñeta (0 = primer nivel).
  pub nivel_vineta: u32,
  /// Sangría izquierda (`w:ind w:left`) convertida a píxeles.
  pub sangria: f32,
  /// El párrafo termina con un salto de página explícito.
  pub salto_pagina: bool,
  /// Si `true`, el párrafo empieza en una página nueva (`w:pageBreakBefore`).
  pub salto_antes: bool,
}

/// Tabla con la cantidad de columnas (de `w:tblGrid`) y sus filas.
pub struct Tabla {
  pub columnas: usize,
  pub filas: Vec<Fila>,
}

/// Fila de una tabla.
pub struct Fila {
  pub celdas: Vec<Celda>,
  /// La fila es un encabezado (`w:trPr/w:tblHeader`).
  pub es_encabezado: bool,
}

/// Celda de una tabla: contiene bloques (párrafos o tablas anidadas).
pub struct Celda {
  pub bloques: Vec<Bloque>,
  /// Extensión horizontal (`w:gridSpan`): columnas que ocupa la celda.
  pub extension: u32,
  /// La celda inicia una fusión vertical (`w:vMerge w:val="restart"`).
  pub fusion_v: bool,
  /// La celda continúa una fusión vertical iniciada arriba (`w:vMerge`).
  pub continua: bool,
  /// Sombreado de fondo (`w:shd w:fill="RRGGBB"`), si hay.
  pub sombreado: Option<[u8; 3]>,
}

impl Default for Parrafo {
  fn default() -> Self {
    Parrafo {
      texto: String::new(),
      nivel_titulo: None,
      vineta: false,
      nivel_vineta: 0,
      sangria: 0.0,
      salto_pagina: false,
      salto_antes: false,
    }
  }
}

impl Default for Tabla {
  fn default() -> Self {
    Tabla { columnas: 0, filas: Vec::new() }
  }
}

impl Default for Fila {
  fn default() -> Self {
    Fila { celdas: Vec::new(), es_encabezado: false }
  }
}

impl Default for Celda {
  fn default() -> Self {
    Celda {
      bloques: Vec::new(),
      extension: 1,
      fusion_v: false,
      continua: false,
      sombreado: None,
    }
  }
}

/// Contenedor en construcción dentro de la pila de parseo.
enum Contenedor {
  Celda(Celda),
  Fila(Fila),
  Tabla(Tabla),
}

/// Extrae el modelo de documento desde un archivo `.docx`.
///
/// Responsabilidad única: decodificar el contenedor OOXML y el XML del
/// cuerpo, sin saber nada sobre cómo se renderiza.
pub struct ExtractorDocx;

impl ExtractorDocx {
  /// Abre el `.docx` (ZIP), lee `word/document.xml` y lo parsea.
  pub fn extraer(ruta: &str) -> Result<DocumentoDocx, String> {
    let archivo =
      std::fs::File::open(ruta).map_err(|e| format!("no se pudo abrir '{ruta}': {e}"))?;
    let mut comprimido = zip::ZipArchive::new(std::io::BufReader::new(archivo))
      .map_err(|e| format!("'{ruta}' no es un archivo ZIP válido: {e}"))?;

    let mut xml = Vec::new();
    comprimido
      .by_name("word/document.xml")
      .map_err(|e| format!("'{ruta}' no contiene word/document.xml (¿es un .docx?): {e}"))?
      .read_to_end(&mut xml)
      .map_err(|e| format!("error al leer word/document.xml: {e}"))?;

    ExtractorDocx::parsear(&xml)
  }

  /// Parseo streaming del cuerpo con una pila de contenedores:
  /// `w:tbl` apila una tabla, `w:tr` una fila y `w:tc` una celda; al
  /// cerrarse cada elemento, su contenido se adjunta al contenedor
  /// superior. Las tablas anidadas funcionan por la propia pila.
  fn parsear(xml: &[u8]) -> Result<DocumentoDocx, String> {
    let texto =
      std::str::from_utf8(xml).map_err(|e| format!("word/document.xml no es UTF-8: {e}"))?;
    let mut lector = Reader::from_str(texto);

    let mut cuerpo: Vec<Bloque> = Vec::new();
    let mut pila: Vec<Contenedor> = Vec::new();
    let mut parrafo: Option<Parrafo> = None;
    let mut en_t = false;
    let mut en_tr_pr = false;
    let mut en_tc_pr = false;
    let mut en_p_pr = false;

    loop {
      match lector.read_event() {
        Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
          "p" => parrafo = Some(Parrafo::default()),
          "pageBreakBefore" => {
            if en_p_pr {
              if let Some(actual) = parrafo.as_mut() {
                actual.salto_antes = true;
              }
            }
          }
          // Salto de línea/página dentro del párrafo (XML: `w:br`, `w:cr`,
          // `w:lastRenderedPageBreak`). `type="page"` o `lastRenderedPageBreak`
          // marcan el fin de la página actual.
          "lastRenderedPageBreak" => {
            if let Some(actual) = parrafo.as_mut() {
              actual.salto_pagina = true;
            }
          }
          "pPr" => en_p_pr = true,
          "trPr" => en_tr_pr = true,
          "tcPr" => en_tc_pr = true,
          "pStyle" => {
            if let Some(actual) = parrafo.as_mut() {
              if let Some(valor) = ExtractorDocx::atributo(&e, "val") {
                actual.nivel_titulo = ExtractorDocx::nivel_de_estilo(&valor);
              }
            }
          }
          "numPr" => {
            if en_p_pr {
              if let Some(actual) = parrafo.as_mut() {
                actual.vineta = true;
              }
            }
          }
          "ilvl" => {
            if en_p_pr {
              if let Some(actual) = parrafo.as_mut() {
                if let Some(valor) = ExtractorDocx::atributo(&e, "val") {
                  actual.nivel_vineta = valor.parse().unwrap_or(0).max(0);
                }
              }
            }
          }
          "ind" => {
            if en_p_pr {
              if let Some(actual) = parrafo.as_mut() {
                if let Some(valor) = ExtractorDocx::atributo(&e, "left") {
                  // twips → píxeles: twips/20 pt × (96/72) px/pt = twips/15.
                  if let Ok(twips) = valor.parse::<f32>() {
                    actual.sangria = (twips / 15.0).max(0.0);
                  }
                }
              }
            }
          }
          "tblHeader" => {
            if en_tr_pr {
              if let Some(Contenedor::Fila(fila)) = pila.last_mut() {
                fila.es_encabezado = true;
              }
            }
          }
          "shd" => {
            if en_tc_pr {
              if let Some(celda) = ExtractorDocx::celda_actual_mut(&mut pila) {
                celda.sombreado = ExtractorDocx::atributo(&e, "fill")
                  .and_then(|fill| ExtractorDocx::color_hex(&fill));
              }
            }
          }
          "t" => en_t = true,
          "tab" => {
            if let Some(actual) = parrafo.as_mut() {
              actual.texto.push(' ');
            }
          }
          "br" | "cr" => {
            if let Some(actual) = parrafo.as_mut() {
              // `br type="page"` = salto de página; el resto = salto de línea.
              if ExtractorDocx::atributo(&e, "type").as_deref() == Some("page") {
                actual.salto_pagina = true;
              } else {
                actual.texto.push('\n');
              }
            }
          }
          "tbl" => pila.push(Contenedor::Tabla(Tabla::default())),
          "tr" => pila.push(Contenedor::Fila(Fila::default())),
          "tc" => pila.push(Contenedor::Celda(Celda::default())),
          "gridSpan" => {
            if let Some(valor) = ExtractorDocx::atributo(&e, "val") {
              if let Some(celda) = ExtractorDocx::celda_actual_mut(&mut pila) {
                celda.extension = valor.parse().unwrap_or(1).max(1);
              }
            }
          }
          "vMerge" => {
            if let Some(celda) = ExtractorDocx::celda_actual_mut(&mut pila) {
              match ExtractorDocx::atributo(&e, "val").as_deref() {
                Some("restart") => celda.fusion_v = true,
                _ => celda.continua = true,
              }
            }
          }
          "gridCol" => {
            if let Some(Contenedor::Tabla(tabla)) = pila.last_mut() {
              tabla.columnas += 1;
            }
          }
          _ => {}
        },
        Ok(Event::Text(t)) => {
          if en_t {
            if let Some(actual) = parrafo.as_mut() {
              let contenido = t.xml10_content().into_owned();
              match quick_xml::escape::unescape(&contenido) {
                Ok(plain) => actual.texto.push_str(&plain),
                Err(_) => actual.texto.push_str(&contenido),
              }
            }
          }
        }
        Ok(Event::End(e)) => match e.local_name().as_ref() {
          "t" => en_t = false,
          "pPr" => en_p_pr = false,
          "trPr" => en_tr_pr = false,
          "tcPr" => en_tc_pr = false,
          "p" => {
            if let Some(actual) = parrafo.take() {
              // `pageBreakBefore`: el párrafo abre una página nueva.
              if actual.salto_antes
                && pila.is_empty()
                && cuerpo.last().is_some_and(|b| !matches!(b, Bloque::SaltoPagina))
              {
                cuerpo.push(Bloque::SaltoPagina);
              }
              if !actual.texto.trim().is_empty() || actual.salto_pagina || actual.salto_antes {
                let salto = actual.salto_pagina;
                if let Some(Contenedor::Celda(celda)) = pila.last_mut() {
                  celda.bloques.push(Bloque::Parrafo(actual));
                } else {
                  cuerpo.push(Bloque::Parrafo(actual));
                }
                // Salto de página explícito al cerrar el párrafo.
                if salto && pila.is_empty() {
                  cuerpo.push(Bloque::SaltoPagina);
                }
              }
            }
          }
          "tc" => {
            if let Some(Contenedor::Celda(celda)) = pila.pop() {
              if let Some(Contenedor::Fila(fila)) = pila.last_mut() {
                fila.celdas.push(celda);
              }
            }
          }
          "tr" => {
            if let Some(Contenedor::Fila(fila)) = pila.pop() {
              if let Some(Contenedor::Tabla(tabla)) = pila.last_mut() {
                tabla.filas.push(fila);
              }
            }
          }
          "tbl" => {
            if let Some(Contenedor::Tabla(tabla)) = pila.pop() {
              let bloque = Bloque::Tabla(tabla);
              if let Some(Contenedor::Celda(celda)) = pila.last_mut() {
                celda.bloques.push(bloque);
              } else {
                cuerpo.push(bloque);
              }
            }
          }
          _ => {}
        },
        Ok(Event::Eof) => break,
        Ok(_) => {}
        Err(e) => return Err(format!("XML malformado en word/document.xml: {e}")),
      }
    }

    Ok(DocumentoDocx { bloques: cuerpo })
  }

  /// Lee el valor de un atributo por su nombre local (`w:val` → `val`).
  fn atributo(
    e: &quick_xml::events::BytesStart<'_>,
    nombre: &str,
  ) -> Option<String> {
    for resultado in e.attributes() {
      if let Ok(a) = resultado {
        if a.key.local_name().as_ref() == nombre {
          return a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .ok()
            .map(|v| v.into_owned());
        }
      }
    }
    None
  }

  /// Convierte un color hexadecimal OOXML (`RRGGBB` o `auto`) en RGB.
  /// Devuelve `None` para `auto` o formatos inválidos.
  fn color_hex(valor: &str) -> Option<[u8; 3]> {
    if valor.len() != 6 || !valor.chars().all(|c| c.is_ascii_hexdigit()) {
      return None;
    }
    let canal = |i: usize| u8::from_str_radix(&valor[i..i + 2], 16).ok();
    Some([
      canal(0)?,
      canal(2)?,
      canal(4)?,
    ])
  }

  /// Convierte un estilo de párrafo en nivel de título: `Heading2` → `2`.
  fn nivel_de_estilo(valor: &str) -> Option<u8> {
    valor
      .strip_prefix("Heading")
      .and_then(|sufijo| sufijo.parse::<u8>().ok())
      .filter(|nivel| (1..=9).contains(nivel))
  }

  /// Devuelve la celda más interna de la pila (la última apilada).
  fn celda_actual_mut(pila: &mut [Contenedor]) -> Option<&mut Celda> {
    pila.iter_mut().rev().find_map(|c| match c {
      Contenedor::Celda(celda) => Some(celda),
      _ => None,
    })
  }
}
