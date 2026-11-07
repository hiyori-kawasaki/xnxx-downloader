use std::sync::LazyLock;

use scraper::{Html, Selector};
use serde_json::Value;

use crate::error::Result;
use crate::http::HttpClient;

#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub title: Option<String>,
    pub thumbnail: Option<String>,
    pub description: Option<String>,
    pub json_ld: Option<Value>,
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("selector CSS constante válido")
}

static META: LazyLock<Selector> = LazyLock::new(|| selector("meta[property]"));
static TITLE: LazyLock<Selector> = LazyLock::new(|| selector("title"));
static JSON_LD: LazyLock<Selector> =
    LazyLock::new(|| selector(r#"script[type="application/ld+json"]"#));

pub fn parse_video(html: &str) -> VideoInfo {
    let doc = Html::parse_document(html);

    // Primera aparición de cada propiedad OpenGraph que tenga `content`.
    let og = |property: &str| -> Option<String> {
        doc.select(&META)
            .filter(|el| el.value().attr("property") == Some(property))
            .find_map(|el| el.value().attr("content"))
            .map(str::to_owned)
    };

    let title_tag = || -> Option<String> {
        doc.select(&TITLE)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_owned())
            .filter(|t| !t.is_empty())
    };

    let json_ld = doc
        .select(&JSON_LD)
        .next()
        .and_then(|el| serde_json::from_str(&el.text().collect::<String>()).ok());

    VideoInfo {
        title: og("og:title").or_else(title_tag),
        thumbnail: og("og:image"),
        description: og("og:description"),
        json_ld,
    }
}

/// Descarga el HTML de la página EN VIVO (genera URLs firmadas nuevas).
pub async fn fetch_page_html(http: &HttpClient, url: &str) -> Result<String> {
    http.get_text(url).await
}

pub async fn get_video_info(http: &HttpClient, url: &str) -> Result<VideoInfo> {
    Ok(parse_video(&fetch_page_html(http, url).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_open_graph_and_falls_back_to_title_tag() {
        let html = r#"<html><head>
            <title> Fallback </title>
            <meta property="og:image" content="http://x/t.jpg">
            <meta property="og:image" content="http://x/ignored.jpg">
            <script type="application/ld+json">{"a":1}</script>
        </head></html>"#;
        let info = parse_video(html);
        assert_eq!(info.title.as_deref(), Some("Fallback"));
        assert_eq!(info.thumbnail.as_deref(), Some("http://x/t.jpg"));
        assert_eq!(info.description, None);
        assert_eq!(info.json_ld, Some(serde_json::json!({ "a": 1 })));
    }

    #[test]
    fn invalid_json_ld_is_ignored() {
        let html = r#"<script type="application/ld+json">{oops</script>"#;
        assert_eq!(parse_video(html).json_ld, None);
    }
}
