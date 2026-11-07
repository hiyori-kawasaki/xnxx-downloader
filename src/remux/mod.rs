//! Remux MPEG-TS (H.264 + AAC) -> MP4 en Rust puro, sin recodificar y sin
//! herramientas externas. El código queda compilado dentro del ejecutable.

mod aac;
mod h264;
mod mp4;
mod pipeline;
mod ts;

use std::path::Path;

use crate::error::{Error, Result};
use pipeline::Pipeline;

/// Abstracción del remux MPEG-TS -> MP4 (Dependency Inversion: el descargador
/// HLS depende de este trait, no de una implementación concreta).
pub trait Remuxer {
    type Session: RemuxSession;

    /// Abre una sesión que recibirá bytes MPEG-TS en orden y producirá `output`.
    fn start(&self, output: &Path) -> Result<Self::Session>;
}

#[allow(async_fn_in_trait)]
pub trait RemuxSession {
    async fn write(&mut self, chunk: &[u8]) -> Result<()>;
    async fn finish(self) -> Result<()>;
}

/// Remux nativo: demux TS + mux MP4 (faststart) sin recodificar.
///
/// Soporta H.264 y AAC (ADTS), que es lo que entrega HLS en la práctica.
/// Otros códecs devuelven [`Error::UnsupportedCodec`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeRemuxer;

impl NativeRemuxer {
    pub fn new() -> Self {
        Self
    }
}

pub struct NativeSession {
    pipeline: Pipeline,
}

impl Remuxer for NativeRemuxer {
    type Session = NativeSession;

    fn start(&self, output: &Path) -> Result<NativeSession> {
        Ok(NativeSession { pipeline: Pipeline::new(output)? })
    }
}

impl RemuxSession for NativeSession {
    async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        self.pipeline.push(chunk)
    }

    async fn finish(self) -> Result<()> {
        // La reescritura final (faststart) copia todo el archivo: fuera del runtime async.
        tokio::task::spawn_blocking(move || self.pipeline.finish())
            .await
            .map_err(|e| Error::Remux(e.to_string()))?
    }
}
