use std::path::{Path, PathBuf};

const MAX_LEN: usize = 150;
const INVALID_CHARS: &str = "<>:\"/\\|?*";
const FALLBACK_NAME: &str = "video";
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Convierte un título en un nombre de archivo válido en Windows y Unix.
pub fn sanitize_filename(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| if c.is_control() || INVALID_CHARS.contains(c) { ' ' } else { c })
        .collect();

    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = collapsed.chars().take(MAX_LEN).collect();
    let name = truncated.trim_matches(|c| c == '.' || c == ' ');

    if name.is_empty() || is_reserved(name) {
        FALLBACK_NAME.to_owned()
    } else {
        name.to_owned()
    }
}

/// Nombres como `CON` o `NUL.txt` están prohibidos en Windows.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem))
}

/// Si `path` ya existe, devuelve `nombre (1).ext`, `nombre (2).ext`, ... para no sobrescribir.
pub fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(FALLBACK_NAME).to_owned();
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_owned);

    (1..)
        .map(|n| match &ext {
            Some(ext) => path.with_file_name(format!("{stem} ({n}).{ext}")),
            None => path.with_file_name(format!("{stem} ({n})")),
        })
        .find(|candidate| !candidate.exists())
        .expect("el rango infinito siempre produce un candidato libre")
}

/// Ruta de salida por defecto: `<título>.mp4` en el directorio actual, sin pisar archivos.
pub fn default_output_path(title: Option<&str>) -> PathBuf {
    let name = sanitize_filename(title.unwrap_or(FALLBACK_NAME));
    unique_path(Path::new(&format!("{name}.mp4")).to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_invalid_characters_and_collapses_spaces() {
        assert_eq!(
            sanitize_filename("ZZ Guide / Roleplay: \"Part 1\"?  <final>"),
            "ZZ Guide Roleplay Part 1 final"
        );
    }

    #[test]
    fn trims_dots_and_falls_back_when_empty() {
        assert_eq!(sanitize_filename("  ...nombre...  "), "nombre");
        assert_eq!(sanitize_filename("???"), "video");
    }

    #[test]
    fn avoids_windows_reserved_names() {
        assert_eq!(sanitize_filename("con"), "video");
        assert_eq!(sanitize_filename("NUL.txt"), "video");
        assert_eq!(sanitize_filename("console"), "console");
    }

    #[test]
    fn truncates_long_titles() {
        let long = "a".repeat(400);
        assert_eq!(sanitize_filename(&long).chars().count(), MAX_LEN);
    }

    #[test]
    fn unique_path_adds_counter_when_file_exists() {
        let dir = std::env::temp_dir().join(format!("video-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("clip.mp4");
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(dir.join("clip (1).mp4"), b"x").unwrap();

        assert_eq!(unique_path(first), dir.join("clip (2).mp4"));
        assert_eq!(unique_path(dir.join("nuevo.mp4")), dir.join("nuevo.mp4"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
