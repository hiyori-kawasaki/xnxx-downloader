use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use futures::stream::{self, StreamExt};
use tokio::sync::Mutex;
use url::Url;

use crate::error::{Error, Result, SegmentFailure};
use crate::http::HttpClient;
use crate::remux::{RemuxSession, Remuxer};
use crate::types::{DownloadedFile, ProgressFn, UrlRefresher};

const MAX_ATTEMPTS: u32 = 5;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);

/// Extrae las URLs absolutas de los segmentos de un playlist de medios.
pub fn parse_segments(playlist: &str, playlist_url: &Url) -> Result<Vec<Url>> {
    let segments = playlist
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.ends_with(".m3u8"))
        .map(|l| playlist_url.join(l))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if segments.is_empty() {
        return Err(Error::NoSegments);
    }
    Ok(segments)
}

async fn fetch_segment_list(http: &HttpClient, playlist_url: &str) -> Result<Vec<Url>> {
    let base = Url::parse(playlist_url)?;
    let body = http.get_text(playlist_url).await?;
    parse_segments(&body, &base)
}

struct Snapshot {
    segments: Vec<Url>,
    generation: u64,
}

/// Lista de segmentos compartida entre descargas concurrentes. Si las URLs
/// firmadas expiran, una sola tarea las renueva (el resto espera y reutiliza).
struct SegmentSource {
    http: HttpClient,
    refresher: Option<UrlRefresher>,
    state: Mutex<Snapshot>,
}

impl SegmentSource {
    fn new(http: HttpClient, segments: Vec<Url>, refresher: Option<UrlRefresher>) -> Self {
        Self {
            http,
            refresher,
            state: Mutex::new(Snapshot { segments, generation: 0 }),
        }
    }

    async fn url_at(&self, index: usize) -> (Url, u64) {
        let state = self.state.lock().await;
        (state.segments[index].clone(), state.generation)
    }

    /// Renueva la lista salvo que otra tarea ya lo hiciera (`seen` obsoleto).
    async fn refresh(&self, seen: u64) -> Result<()> {
        let Some(refresher) = &self.refresher else {
            return Ok(());
        };
        let mut state = self.state.lock().await;
        if state.generation != seen {
            return Ok(());
        }
        let master = refresher().await?;
        let fresh = fetch_segment_list(&self.http, &master).await?;
        if fresh.len() == state.segments.len() {
            state.segments = fresh;
        }
        state.generation += 1;
        Ok(())
    }

    async fn fetch(&self, index: usize) -> Result<Bytes> {
        let mut cause = SegmentFailure::Network;

        for attempt in 0..MAX_ATTEMPTS {
            let (url, generation) = self.url_at(index).await;
            match self.http.try_get(url.as_str()).await {
                Ok(response) if response.status().is_success() => match response.bytes().await {
                    Ok(bytes) => return Ok(bytes),
                    Err(_) => cause = SegmentFailure::Network,
                },
                Ok(response) => {
                    let status = response.status().as_u16();
                    cause = SegmentFailure::Status(status);
                    if matches!(status, 403 | 410) {
                        // URL firmada expirada: un fallo al renovar se reintenta en la vuelta siguiente.
                        if let Err(e) = self.refresh(generation).await {
                            log::debug!("no se pudo renovar las URLs: {e}");
                        }
                    }
                }
                Err(_) => cause = SegmentFailure::Network,
            }
            tokio::time::sleep(RETRY_BASE_DELAY * (attempt + 1)).await;
        }

        Err(Error::Segment { index, cause })
    }
}

/// Descarga HLS: segmentos en paralelo (con backpressure y orden garantizado)
/// enviados en streaming al remuxer.
pub struct HlsDownloader<R: Remuxer> {
    http: HttpClient,
    remuxer: R,
    concurrency: usize,
}

impl<R: Remuxer> HlsDownloader<R> {
    pub fn new(http: HttpClient, remuxer: R, concurrency: usize) -> Self {
        Self { http, remuxer, concurrency: concurrency.max(1) }
    }

    pub async fn download(
        &self,
        playlist_url: &str,
        out: &Path,
        progress: Option<&ProgressFn>,
        refresher: Option<UrlRefresher>,
    ) -> Result<DownloadedFile> {
        let segments = fetch_segment_list(&self.http, playlist_url).await?;
        let total = segments.len();
        let source = SegmentSource::new(self.http.clone(), segments, refresher);

        self.pump(&source, total, out, progress).await?;

        let bytes = tokio::fs::metadata(out).await?.len();
        Ok(DownloadedFile { path: out.to_path_buf(), bytes })
    }

    async fn pump(
        &self,
        source: &SegmentSource,
        total: usize,
        out: &Path,
        progress: Option<&ProgressFn>,
    ) -> Result<()> {
        let mut session = self.remuxer.start(out)?;

        // `buffered` ejecuta N descargas a la vez y entrega los resultados en orden.
        let mut chunks = stream::iter(0..total)
            .map(|index| source.fetch(index))
            .buffered(self.concurrency);

        let mut done = 0usize;
        while let Some(chunk) = chunks.next().await {
            session.write(&chunk?).await?;
            done += 1;
            if let Some(report) = progress {
                report(done as f64 / total as f64);
            }
        }
        drop(chunks);

        session.finish().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_segment_lines() {
        let playlist = "#EXTM3U\n#EXTINF:4,\nseg0.ts\r\n#EXTINF:4,\n/seg1.ts?x=1\n#EXT-X-ENDLIST\n";
        let base = Url::parse("https://cdn.test/v/media.m3u8").unwrap();
        let segments = parse_segments(playlist, &base).unwrap();
        assert_eq!(
            segments.iter().map(Url::as_str).collect::<Vec<_>>(),
            ["https://cdn.test/v/seg0.ts", "https://cdn.test/seg1.ts?x=1"]
        );
    }

    #[test]
    fn master_playlist_has_no_segments() {
        let base = Url::parse("https://cdn.test/master.m3u8").unwrap();
        let err = parse_segments("#EXTM3U\nlow.m3u8\n", &base).unwrap_err();
        assert!(matches!(err, Error::NoSegments));
    }
}
