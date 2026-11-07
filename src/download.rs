use std::path::Path;

use futures::StreamExt;
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::error::Result;
use crate::hls::HlsDownloader;
use crate::http::HttpClient;
use crate::remux::Remuxer;
use crate::types::{DownloadedFile, ProgressFn, UrlRefresher};

const WRITE_BUFFER: usize = 256 * 1024;

#[derive(Default, Clone)]
pub struct DownloadOptions {
    pub progress: Option<ProgressFn>,
    pub refresher: Option<UrlRefresher>,
}

fn is_hls(url: &str) -> bool {
    url.contains(".m3u8")
}

/// Descarga directa en streaming (sin cargar el archivo en memoria).
pub async fn download_file(
    http: &HttpClient,
    url: &str,
    out: &Path,
    progress: Option<&ProgressFn>,
) -> Result<DownloadedFile> {
    let response = http.get(url).await?;
    let total = response.content_length().filter(|&n| n > 0);
    let mut file = BufWriter::with_capacity(WRITE_BUFFER, File::create(out).await?);
    let mut body = response.bytes_stream();
    let mut received = 0u64;

    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        received += chunk.len() as u64;
        if let (Some(total), Some(report)) = (total, progress) {
            report(received as f64 / total as f64);
        }
    }
    file.flush().await?;

    Ok(DownloadedFile { path: out.to_path_buf(), bytes: received })
}

/// Punto de entrada unificado: elige HLS o descarga directa según la URL.
pub struct Downloader<R: Remuxer> {
    http: HttpClient,
    hls: HlsDownloader<R>,
}

impl<R: Remuxer> Downloader<R> {
    pub fn new(http: HttpClient, remuxer: R, concurrency: usize) -> Self {
        let hls = HlsDownloader::new(http.clone(), remuxer, concurrency);
        Self { http, hls }
    }

    pub async fn download(
        &self,
        url: &str,
        out: &Path,
        options: DownloadOptions,
    ) -> Result<DownloadedFile> {
        let DownloadOptions { progress, refresher } = options;
        let result = if is_hls(url) {
            self.hls.download(url, out, progress.as_ref(), refresher).await
        } else {
            download_file(&self.http, url, out, progress.as_ref()).await
        };

        if result.is_err() {
            // No dejar archivos parciales/corruptos.
            let _ = tokio::fs::remove_file(out).await;
        }
        result
    }
}
