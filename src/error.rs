use std::fmt;

/// Causa del último fallo al descargar un segmento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentFailure {
    Status(u16),
    Network,
}

impl fmt::Display for SegmentFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(code) => write!(f, "HTTP {code}"),
            Self::Network => f.write_str("error de red"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP {status} en {url}")]
    HttpStatus { status: u16, url: String },

    #[error("error de red: {0}")]
    Network(#[from] reqwest::Error),

    #[error("URL inválida: {0}")]
    InvalidUrl(#[from] url::ParseError),

    #[error("error de E/S: {0}")]
    Io(#[from] std::io::Error),

    #[error("No se encontraron segmentos HLS válidos")]
    NoSegments,

    #[error("Fallo segmento {index}: {cause}")]
    Segment { index: usize, cause: SegmentFailure },

    #[error("el remux falló: {0}")]
    Remux(String),

    #[error("códec no soportado en el MPEG-TS: {0}")]
    UnsupportedCodec(String),

    #[error("resolución '{0}' no disponible")]
    ResolutionNotFound(String),

    #[error("la página no expone ninguna resolución descargable")]
    NoResolutions,
}

pub type Result<T> = std::result::Result<T, Error>;
