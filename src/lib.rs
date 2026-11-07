//! Descarga de videos (MP4 directo o HLS) con remux a MP4 en streaming.

pub mod download;
pub mod error;
pub mod filename;
pub mod hls;
pub mod http;
pub mod player;
pub mod remux;
pub mod types;
pub mod video_info;

pub use download::{download_file, DownloadOptions, Downloader};
pub use error::{Error, Result};
pub use filename::{default_output_path, sanitize_filename, unique_path};
pub use hls::HlsDownloader;
pub use http::HttpClient;
pub use player::{
    get_resolutions, parse_hls_master, parse_player_urls, PlayerUrls, Resolution, Resolutions,
};
pub use remux::{NativeRemuxer, RemuxSession, Remuxer};
pub use types::{DownloadedFile, ProgressFn, UrlRefresher};
pub use video_info::{fetch_page_html, get_video_info, parse_video, VideoInfo};
