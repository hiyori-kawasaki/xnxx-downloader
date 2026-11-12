//! Lista los videos de un perfil/modelo.
//!
//! La página del perfil solo trae el esqueleto: la pestaña "Latest" se carga
//! por AJAX desde `<perfil>/activity/<filtro>[/<n>]`, por eso un GET normal a la
//! página no ve los videos de abajo. Aquí se piden esas páginas una a una.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use url::Url;

use crate::error::Result;
use crate::http::HttpClient;
use crate::video_info::fetch_page_html;

const PAGE_DELAY: Duration = Duration::from_millis(300);
const DEFAULT_FILTER: &str = "straight";

static PAGE_FILTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""pagefilter":"(\w+)""#).expect("regex constante válida"));

/// `/video.<id>/<slug>`, `/video-<id>/<slug>` o `/video<digitos>/<slug>`.
static VIDEO_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"/video(?:\.[A-Za-z0-9]+|-[A-Za-z0-9]+|\d+)/[\w.~%-]+")
        .expect("regex constante válida")
});

/// El fragmento AJAX puede empezar con `<!-- N -->` (siguiente página).
static NEXT_PAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*<!--\s*(\d+)\s*-->").expect("regex constante válida"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoLink {
    pub id: String,
    pub title: String,
    pub url: String,
}

/// Filtro de contenido de la página (`straight`, `gay`, `shemale`).
pub fn parse_page_filter(html: &str) -> String {
    PAGE_FILTER
        .captures(html)
        .map(|c| c[1].to_owned())
        .unwrap_or_else(|| DEFAULT_FILTER.to_owned())
}

/// Extrae enlaces a videos de un HTML/fragmento. Mira el texto completo (no solo
/// los `<a>`) porque parte de las miniaturas llega dentro de `<script>`.
/// Deduplica por id conservando el orden de aparición.
pub fn extract_video_links(text: &str, base: &Url) -> Vec<VideoLink> {
    let text = text.replace("\\/", "/");
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for m in VIDEO_PATH.find_iter(&text) {
        let path = m.as_str().trim_end_matches('.');
        let mut parts = path.trim_start_matches('/').splitn(2, '/');
        let (Some(id), Some(slug)) = (parts.next(), parts.next()) else {
            continue;
        };
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let Ok(url) = base.join(path) else { continue };
        out.push(VideoLink {
            id: id.to_owned(),
            title: slug.replace('_', " "),
            url: url.to_string(),
        });
    }
    out
}

/// Recorre la pestaña "Latest" del perfil hasta que una página no aporte videos
/// nuevos (o se llegue a `max_pages`).
pub async fn list_profile_videos(
    http: &HttpClient,
    profile_url: &str,
    max_pages: usize,
) -> Result<Vec<VideoLink>> {
    let mut base = Url::parse(profile_url)?;
    base.set_fragment(None);
    base.set_query(None);
    let profile = base.as_str().trim_end_matches('/').to_owned();

    let html = fetch_page_html(http, &profile).await?;
    let filter = parse_page_filter(&html);

    // Los videos que ya vienen en la propia página (pestaña "About me").
    let mut seen = HashSet::new();
    let mut all: Vec<VideoLink> = extract_video_links(&html, &base)
        .into_iter()
        .filter(|v| seen.insert(v.id.clone()))
        .collect();

    let mut page = 0usize;
    for _ in 0..max_pages {
        let url = if page == 0 {
            format!("{profile}/activity/{filter}")
        } else {
            format!("{profile}/activity/{filter}/{page}")
        };

        let body = match http.get_text_xhr(&url).await {
            Ok(body) => body,
            Err(e) if page > 0 => {
                log::warn!("página {page}: {e}");
                break;
            }
            Err(e) => return Err(e),
        };

        let fresh: Vec<VideoLink> = extract_video_links(&body, &base)
            .into_iter()
            .filter(|v| seen.insert(v.id.clone()))
            .collect();
        log::debug!("{url}: {} videos nuevos", fresh.len());
        if fresh.is_empty() {
            break;
        }
        all.extend(fresh);

        page = NEXT_PAGE
            .captures(&body)
            .and_then(|c| c[1].parse::<usize>().ok())
            .filter(|&n| n > page)
            .unwrap_or(page + 1);
        tokio::time::sleep(PAGE_DELAY).await;
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_unique_video_links_including_escaped_ones() {
        let base = Url::parse("https://www.xvideos.com/models/x").unwrap();
        let text = r#"<a href="/video.abc123/my_title_one">a</a>
            <script>var h = "<a href=\"\/video.def456\/other_title\">";</script>
            <a href="/video.abc123/my_title_one">dup</a>
            <a href="/videos-i-like">no</a>"#;
        let links = extract_video_links(text, &base);
        assert_eq!(
            links.iter().map(|l| l.url.as_str()).collect::<Vec<_>>(),
            [
                "https://www.xvideos.com/video.abc123/my_title_one",
                "https://www.xvideos.com/video.def456/other_title"
            ]
        );
        assert_eq!(links[0].title, "my title one");
    }

    #[test]
    fn reads_page_filter_with_fallback() {
        assert_eq!(parse_page_filter(r#"{"dyn":{"pagefilter":"gay"}}"#), "gay");
        assert_eq!(parse_page_filter("<html></html>"), "straight");
    }

    #[test]
    fn next_page_comment_is_parsed() {
        let c = NEXT_PAGE.captures("<!-- 3 -->\n<div>").unwrap();
        assert_eq!(&c[1], "3");
    }
}
