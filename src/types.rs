use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::error::Result;

/// Callback de progreso (fracción entre 0.0 y 1.0).
pub type ProgressFn = Arc<dyn Fn(f64) + Send + Sync>;

/// Callback que devuelve una URL maestra nueva cuando las firmadas expiran.
pub type UrlRefresher = Arc<dyn Fn() -> BoxFuture<'static, Result<String>> + Send + Sync>;

/// Resultado de una descarga.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedFile {
    pub path: PathBuf,
    pub bytes: u64,
}
