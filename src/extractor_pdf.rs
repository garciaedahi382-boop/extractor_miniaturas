//! Extractor de miniaturas desde documentos PDF, implementado sobre
//! [pdfium-render] (renderizado) e [image] (codificación PNG).

use image::DynamicImage;
use pdfium_render::prelude::*;
use std::sync::OnceLock;

/// Ancho fijo en píxeles de la miniatura; el alto se deriva de la
/// proporción real de la página para no deformarla.
const ANCHO_MINIATURA: u16 = 1024;

/// Instancia única de pdfium por proceso: la librería nativa solo puede
/// inicializarse una vez (un segundo intento falla con
/// `PdfiumLibraryBindingsAlreadyInitialized`), así que se carga perezosamente
/// y se reutiliza. `Pdfium` implementa `Send + Sync`.
static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

/// Renderiza páginas de un documento PDF como imágenes raster.
///
/// Responsabilidad única: rasterizar PDF, sin saber cómo se guardan
/// ni desde qué flujo proviene la ruta.
pub struct ExtractorPdf;

impl ExtractorPdf {
  /// Devuelve la instancia única de pdfium del proceso, cargándola en el
  /// primer uso y recordando el error si la librería no está disponible.
  fn instancia() -> Result<&'static Pdfium, String> {
    PDFIUM
      .get_or_init(|| {
        Pdfium::bind_to_system_library()
          .map(&Pdfium::new)
          .map_err(|e| format!("no se pudo cargar la librería de pdfium: {e}"))
      })
      .as_ref()
      .map_err(|e| e.clone())
  }

  /// Renderiza todas las páginas del documento en `ruta` y guarda cada una
  /// como PNG. La ruta de salida de cada página se deriva de `ruta_salida`
  /// agregando el número de página (base 1): para `mini.png` genera
  /// `mini_p1.png`, `mini_p2.png`, … en la misma carpeta.
  ///
  /// Devuelve la cantidad de páginas renderizadas.
  ///
  /// Renderiza todas las páginas del documento en `ruta` a imágenes en
  /// memoria, sin escribir archivos.
  ///
  /// Requiere que la librería dinámica de pdfium (`pdfium.dll` en Windows,
  /// `libpdfium.so` en Linux) esté disponible en la ruta de búsqueda del
  /// sistema.
  pub fn paginas(ruta: &str) -> Result<Vec<DynamicImage>, String> {
    let pdfium = ExtractorPdf::instancia()?;

    let documento = pdfium
      .load_pdf_from_file(ruta, None)
      .map_err(|e| format!("no se pudo abrir '{ruta}': {e}"))?;

    let paginas = documento.pages();
    if paginas.is_empty() {
      return Err(format!("'{ruta}' no contiene páginas"));
    }

    (0..paginas.len())
      .map(|indice| {
        let pagina = paginas
          .get(indice)
          .map_err(|e| format!("error al acceder a la página {}: {e}", indice + 1))?;
        ExtractorPdf::renderizar_pagina(pagina)
      })
      .collect()
  }

  /// Renderiza todas las páginas del documento en `ruta` y guarda cada una
  /// como PNG derivando la ruta de `ruta_salida` con el número de página.
  /// Devuelve la cantidad de páginas renderizadas.
  pub fn renderizar_paginas(ruta: &str, ruta_salida: &str) -> Result<u32, String> {
    let imagenes = ExtractorPdf::paginas(ruta)?;
    for (indice, imagen) in imagenes.iter().enumerate() {
      let salida =
        crate::NombradorSalida::derivar(ruta_salida, &format!("p{}", indice + 1));
      imagen
        .save(&salida)
        .map_err(|e| format!("error al escribir '{salida}': {e}"))?;
    }
    Ok(imagenes.len() as u32)
  }

  /// Rasteriza una sola página a la altura proporcional a su tamaño real.
  fn renderizar_pagina(pagina: PdfPage<'_>) -> Result<DynamicImage, String> {
    // Alto proporcional al tamaño real de la página.
    let ancho_pagina = pagina.width().value.max(1.0);
    let alto_pagina = pagina.height().value;
    let escala = ANCHO_MINIATURA as f32 / ancho_pagina;
    let alto_miniatura = ((alto_pagina * escala).round() as i32).max(1);

    let bitmap = pagina
      .render_with_config(
        &PdfRenderConfig::new().set_target_size(ANCHO_MINIATURA as i32, alto_miniatura),
      )
      .map_err(|e| format!("error al rasterizar la página: {e}"))?;

    bitmap
      .as_image()
      .map_err(|e| format!("error al convertir el bitmap: {e}"))
  }
}
