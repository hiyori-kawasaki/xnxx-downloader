
use std::error::Error as StdError;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use dialoguer::{theme::ColorfulTheme, Select};
use downloader_xnxx::{
    default_output_path, fetch_page_html, get_resolutions, get_video_info, parse_video,
    DownloadOptions, Downloader, Error, HttpClient, NativeRemuxer, ProgressFn, Resolution, Resolutions,
    UrlRefresher,
};

#[derive(Parser)]
#[command(version, about = "Descarga videos MP4/HLS")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Muestra título, miniatura y descripción de la página.
    Info { url: String },
    /// Lista las resoluciones disponibles.
    Resolutions { url: String },
    /// Descarga el video a un archivo MP4.
    Download {
        url: String,
        /// Archivo de salida. Si se omite, se usa el título del video (`<título>.mp4`).
        out: Option<PathBuf>,
        /// Nombre de la resolución (p. ej. hls-720p, mp4-high). Si se omite, se muestra
        /// un menú para elegir (o se usa la mejor si no hay terminal interactiva).
        #[arg(short, long)]
        quality: Option<String>,
        /// Usa la mejor calidad sin preguntar.
        #[arg(short, long, conflicts_with = "quality")]
        best: bool,
        /// Descargas de segmentos en paralelo.
        #[arg(short, long, default_value_t = 8)]
        threads: usize,
    },
    /// Imprime el script de autocompletado para tu shell (powershell, bash, zsh, fish).
    Completions { shell: Shell },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn StdError>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();

    // No necesita red: se resuelve antes de crear el cliente HTTP.
    if let Command::Completions { shell } = cli.command {
        clap_complete::generate(shell, &mut Cli::command(), "video-dl", &mut std::io::stdout());
        return Ok(());
    }

    let http = HttpClient::new()?;

    match cli.command {
        Command::Completions { .. } => unreachable!("tratado arriba"),
        Command::Info { url } => {
            let info = get_video_info(&http, &url).await?;
            println!("título:      {}", info.title.as_deref().unwrap_or("-"));
            println!("miniatura:   {}", info.thumbnail.as_deref().unwrap_or("-"));
            println!("descripción: {}", info.description.as_deref().unwrap_or("-"));
            if let Some(ld) = info.json_ld {
                println!("json-ld:\n{}", serde_json::to_string_pretty(&ld)?);
            }
        }
        Command::Resolutions { url } => {
            let html = fetch_page_html(&http, &url).await?;
            for r in get_resolutions(&http, &html).await?.iter() {
                println!("{:<12} {}", r.name, r.url);
            }
        }
        Command::Download { url, out, quality, best, threads } => {
            let html = fetch_page_html(&http, &url).await?;
            let out = out.unwrap_or_else(|| default_output_path(parse_video(&html).title.as_deref()));
            let resolutions = get_resolutions(&http, &html).await?;
            let chosen = match &quality {
                Some(name) => resolutions
                    .get(name)
                    .ok_or_else(|| Error::ResolutionNotFound(name.clone()))?
                    .clone(),
                None if best || !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) => {
                    resolutions.best().ok_or(Error::NoResolutions)?.clone()
                }
                None => choose_resolution(&resolutions)?,
            };

            let progress: ProgressFn = Arc::new(|p| eprint!("\r{:5.1}%", p * 100.0));
            let refresher = page_refresher(http.clone(), url, chosen.name.clone());

            let downloader = Downloader::new(http, NativeRemuxer::new(), threads);
            let file = downloader
                .download(
                    &chosen.url,
                    &out,
                    DownloadOptions { progress: Some(progress), refresher: Some(refresher) },
                )
                .await?;
            eprintln!();
            println!("{} ({} mb)", file.path.display(),  (file.bytes) as f64 / 1024.0 / 1024.0);
        }
    }
    Ok(())
}

/// Re-lee la página en vivo y devuelve la URL fresca de la misma resolución.
fn page_refresher(http: HttpClient, page_url: String, name: String) -> UrlRefresher {
    Arc::new(move || {
        let (http, page_url, name) = (http.clone(), page_url.clone(), name.clone());
        Box::pin(async move {
            let html = fetch_page_html(&http, &page_url).await?;
            let resolutions = get_resolutions(&http, &html).await?;
            resolutions
                .get(&name)
                .map(|r| r.url.clone())
                .ok_or(Error::ResolutionNotFound(name))
        })
    })
}

/// Orden de la lista: mayor altura primero; a igual altura, HLS antes que MP4.
fn rank(r: &Resolution) -> (u32, bool) {
    match r.hls_height() {
        Some(h) => (h as u32, true),
        None => match r.name.as_str() {
            "mp4-high" => (360, false),
            "mp4-low" => (240, false),
            _ => (0, false),
        },
    }
}

/// Menú interactivo: ↑/↓ para moverse, Enter para elegir, Esc para cancelar.
fn choose_resolution(resolutions: &Resolutions) -> Result<Resolution, Box<dyn StdError>> {
    let best = resolutions.best().ok_or(Error::NoResolutions)?;
    let mut list: Vec<&Resolution> = resolutions.iter().collect();
    list.sort_by_key(|r| std::cmp::Reverse(rank(r)));

    let items: Vec<String> = list
        .iter()
        .map(|r| if r.name == best.name { format!("{}  (mejor)", r.name) } else { r.name.clone() })
        .collect();
    let default = list.iter().position(|r| r.name == best.name).unwrap_or(0);

    let picked = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Elige la calidad (↑/↓ y Enter)")
        .items(&items)
        .default(default)
        .interact_opt()?;

    match picked {
        Some(i) => Ok(list[i].clone()),
        None => Err("selección cancelada".into()),
    }
}