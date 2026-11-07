use std::sync::LazyLock;

use regex::Regex;
use url::Url;

use crate::error::Result;
use crate::http::HttpClient;

static PLAYER_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"setVideo(UrlLow|UrlHigh|HLS)\(\s*['"]([^'"]+)['"]\s*\)"#)
        .expect("regex constante válida")
});

static STREAM_HEIGHT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"RESOLUTION=\d+x(\d+)").expect("regex constante válida"));

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PlayerUrls {
    pub low: Option<String>,
    pub high: Option<String>,
    pub hls: Option<String>,
}

/// Extrae las URLs de `setVideoUrlLow/High/HLS(...)`; gana la primera aparición.
pub fn parse_player_urls(html: &str) -> PlayerUrls {
    let mut urls = PlayerUrls::default();
    for caps in PLAYER_CALL.captures_iter(html) {
        let slot = match &caps[1] {
            "UrlLow" => &mut urls.low,
            "UrlHigh" => &mut urls.high,
            _ => &mut urls.hls,
        };
        slot.get_or_insert_with(|| caps[2].to_owned());
    }
    urls
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub name: String,
    pub url: String,
}

impl Resolution {
    /// Altura en píxeles para nombres tipo `hls-720p`.
    pub fn hls_height(&self) -> Option<u32> {
        self.name.strip_prefix("hls-")?.strip_suffix('p')?.parse().ok()
    }
}

/// Colección ordenada (orden de inserción) de resoluciones disponibles.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Resolutions(Vec<Resolution>);

impl Resolutions {
    /// Inserta o reemplaza por nombre, preservando el orden original.
    pub fn insert(&mut self, name: impl Into<String>, url: impl Into<String>) {
        let (name, url) = (name.into(), url.into());
        match self.0.iter_mut().find(|r| r.name == name) {
            Some(existing) => existing.url = url,
            None => self.0.push(Resolution { name, url }),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Resolution> {
        self.0.iter().find(|r| r.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Resolution> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Mejor calidad: mayor HLS; si no hay, `mp4-high`, luego `mp4-low`.
    pub fn best(&self) -> Option<&Resolution> {
        self.0
            .iter()
            .filter(|r| r.hls_height().is_some())
            .max_by_key(|r| r.hls_height())
            .or_else(|| self.get("mp4-high"))
            .or_else(|| self.get("mp4-low"))
    }
}

/// Convierte un playlist HLS maestro en resoluciones `hls-<alto>p`.
pub fn parse_hls_master(m3u8: &str, master_url: &Url) -> Resolutions {
    let mut out = Resolutions::default();
    let mut lines = m3u8.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("#EXT-X-STREAM-INF") {
            continue;
        }
        let height = STREAM_HEIGHT.captures(line).map(|c| c[1].to_owned());
        let next = lines.peek().map(|l| l.trim()).filter(|l| !l.is_empty());
        let (Some(height), Some(next)) = (height, next) else {
            continue;
        };
        if let Ok(url) = master_url.join(next) {
            out.insert(format!("hls-{height}p"), url);
        }
    }
    out
}

pub async fn get_resolutions(http: &HttpClient, html: &str) -> Result<Resolutions> {
    let PlayerUrls { low, high, hls } = parse_player_urls(html);
    let mut resolutions = Resolutions::default();
    if let Some(url) = low {
        resolutions.insert("mp4-low", url);
    }
    if let Some(url) = high {
        resolutions.insert("mp4-high", url);
    }
    match hls {
        Some(master) => {
            let master_url = Url::parse(&master)?;
            match http.get_text(master.as_str()).await {
                Ok(body) => {
                    for r in parse_hls_master(&body, &master_url).0 {
                        resolutions.insert(r.name, r.url);
                    }
                }
                Err(e) => log::warn!("No se pudo leer el playlist HLS {master}: {e}"),
            }
        }
        None => log::warn!("No se encontró setVideoHLS(...) en el HTML"),
    }
    Ok(resolutions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_first_player_urls() {
        let html = r#"setVideoUrlLow('http://a/low.mp4'); setVideoHLS("http://a/m.m3u8");
                      setVideoUrlLow('http://b/ignored.mp4');"#;
        let urls = parse_player_urls(html);
        assert_eq!(urls.low.as_deref(), Some("http://a/low.mp4"));
        assert_eq!(urls.high, None);
        assert_eq!(urls.hls.as_deref(), Some("http://a/m.m3u8"));
    }

    #[test]
    fn parses_master_playlist_with_relative_urls() {
        let m3u8 = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720\nhls-720.m3u8\n\
                    #EXT-X-STREAM-INF:BANDWIDTH=2,RESOLUTION=1920x1080\n/abs/hls-1080.m3u8\n";
        let base = Url::parse("https://cdn.test/path/master.m3u8").unwrap();
        let res = parse_hls_master(m3u8, &base);
        assert_eq!(res.get("hls-720p").unwrap().url, "https://cdn.test/path/hls-720.m3u8");
        assert_eq!(res.get("hls-1080p").unwrap().url, "https://cdn.test/abs/hls-1080.m3u8");
        assert_eq!(res.best().unwrap().name, "hls-1080p");
    }

    #[test]
    fn best_falls_back_to_mp4() {
        let mut res = Resolutions::default();
        res.insert("mp4-low", "l");
        res.insert("mp4-high", "h");
        assert_eq!(res.best().unwrap().name, "mp4-high");
    }
}