//! FreeDesktop .desktop application discovery and launching for Linux desktops.
//! Parses `/usr/share/applications/` and `~/.local/share/applications/`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A desktop application declared by an XDG `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    /// File basename, e.g. `audacity.desktop` or `code.desktop`.
    pub id: String,
    /// Human-friendly display name.
    pub name: String,
    /// Exec template string (e.g. `audacity %F`).
    pub exec: String,
    /// Icon name or absolute path from the Icon= field.
    pub icon: Option<String>,
    /// Supported MIME types.
    pub mime_types: Vec<String>,
    /// Whether this app runs in a terminal window.
    pub terminal: bool,
    /// Whether this entry has `NoDisplay=true`.
    pub no_display: bool,
}

impl DesktopApp {
    /// Parses a FreeDesktop `.desktop` file format.
    pub fn parse_str(id: &str, content: &str) -> Option<Self> {
        let mut in_desktop_entry = false;
        let mut name: Option<String> = None;
        let mut localized_name: Option<String> = None;
        let mut exec: Option<String> = None;
        let mut icon: Option<String> = None;
        let mut mime_types = Vec::new();
        let mut terminal = false;
        let mut no_display = false;
        let mut is_application = false;

        let lang = std::env::var("LANG").unwrap_or_default();
        let is_spanish = lang.starts_with("es");

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                in_desktop_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_desktop_entry {
                continue;
            }

            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim();
                let val = v.trim();
                match key {
                    "Type" => {
                        if val.eq_ignore_ascii_case("Application") {
                            is_application = true;
                        }
                    }
                    "Name" => {
                        if name.is_none() {
                            name = Some(val.to_string());
                        }
                    }
                    "Name[es]" if is_spanish => {
                        localized_name = Some(val.to_string());
                    }
                    "Exec" => {
                        exec = Some(val.to_string());
                    }
                    "Icon" => {
                        icon = Some(val.to_string());
                    }
                    "MimeType" => {
                        for mime in val.split(';') {
                            let mime = mime.trim();
                            if !mime.is_empty() {
                                mime_types.push(mime.to_lowercase());
                            }
                        }
                    }
                    "Terminal" => {
                        terminal = val.eq_ignore_ascii_case("true") || val == "1";
                    }
                    "NoDisplay" => {
                        no_display = val.eq_ignore_ascii_case("true") || val == "1";
                    }
                    _ => {}
                }
            }
        }

        if !is_application || no_display {
            return None;
        }

        let final_name = localized_name.or(name)?;
        let exec = exec?;

        Some(Self {
            id: id.to_string(),
            name: final_name,
            exec,
            icon,
            mime_types,
            terminal,
            no_display,
        })
    }

    /// Parse a single desktop file from disk.
    pub fn parse_file(path: &Path) -> Option<Self> {
        let id = path.file_name()?.to_string_lossy().to_string();
        let content = std::fs::read_to_string(path).ok()?;
        Self::parse_str(&id, &content)
    }
}

/// Standard XDG application directories to search.
pub fn xdg_application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/applications"));
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs
}

static APPS_CACHE: OnceLock<Arc<Mutex<Option<(Instant, Vec<DesktopApp>)>>>> = OnceLock::new();

fn apps_cache_holder() -> Arc<Mutex<Option<(Instant, Vec<DesktopApp>)>>> {
    APPS_CACHE
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Lists all installed system desktop applications, cached for 10 seconds.
pub fn list_system_apps() -> Vec<DesktopApp> {
    let holder = apps_cache_holder();
    let mut lock = holder.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, ref apps)) = *lock {
        if at.elapsed() < Duration::from_secs(10) {
            return apps.clone();
        }
    }

    let mut seen_ids = HashSet::new();
    let mut apps = Vec::new();

    for dir in xdg_application_dirs() {
        if !dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "desktop") {
                let id = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if seen_ids.insert(id) {
                    if let Some(app) = DesktopApp::parse_file(&p) {
                        apps.push(app);
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    *lock = Some((Instant::now(), apps.clone()));
    apps
}

/// Maps a file extension to standard MIME types.
pub fn ext_to_mime_types(ext: &str) -> &'static [&'static str] {
    match ext.to_ascii_lowercase().as_str() {
        // Images
        "png" => &["image/png"],
        "jpg" | "jpeg" => &["image/jpeg"],
        "webp" => &["image/webp"],
        "gif" => &["image/gif"],
        "svg" => &["image/svg+xml"],
        "bmp" => &["image/bmp", "image/x-ms-bmp"],
        "ico" => &["image/x-icon", "image/vnd.microsoft.icon"],
        "tiff" | "tif" => &["image/tiff"],
        // Audio
        "mp3" => &["audio/mpeg", "audio/mp3", "audio/x-mp3"],
        "flac" => &["audio/flac", "audio/x-flac"],
        "wav" => &["audio/wav", "audio/x-wav"],
        "ogg" | "oga" | "opus" => &["audio/ogg", "audio/opus"],
        "m4a" | "aac" => &["audio/mp4", "audio/aac"],
        // Video
        "mp4" | "m4v" => &["video/mp4"],
        "mkv" => &["video/x-matroska"],
        "webm" => &["video/webm"],
        "avi" => &["video/x-msvideo", "video/avi"],
        "mov" => &["video/quicktime"],
        // Documents
        "pdf" => &["application/pdf"],
        "epub" => &["application/epub+zip"],
        // Text / Code
        "txt" => &["text/plain"],
        "md" | "markdown" => &["text/markdown", "text/plain"],
        "rs" => &["text/rust", "text/x-rust", "text/plain"],
        "c" | "h" => &["text/x-c", "text/plain"],
        "cpp" | "hpp" | "cc" | "cxx" => &["text/x-c++", "text/plain"],
        "py" => &["text/x-python", "text/plain"],
        "js" | "mjs" | "cjs" | "jsx" => {
            &["text/javascript", "application/javascript", "text/plain"]
        }
        "ts" | "mts" | "cts" | "tsx" => {
            &["text/typescript", "application/typescript", "text/plain"]
        }
        "json" => &["application/json", "text/plain"],
        "toml" => &["application/toml", "text/plain"],
        "yaml" | "yml" => &["application/yaml", "text/plain"],
        "sh" | "bash" | "zsh" => &["application/x-shellscript", "text/plain"],
        "html" | "htm" => &["text/html"],
        "css" => &["text/css"],
        "xml" => &["application/xml", "text/xml"],
        // Archives
        "zip" => &["application/zip"],
        "tar" => &["application/x-tar"],
        "gz" => &["application/gzip"],
        "7z" => &["application/x-7z-compressed"],
        "rar" => &["application/vnd.rar", "application/x-rar"],
        _ => &[],
    }
}

/// Finds applications associated with the given file path based on its extension and MIME types.
pub fn apps_for_file(path: &Path) -> Vec<DesktopApp> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let file_mimes = ext_to_mime_types(ext);
    if file_mimes.is_empty() {
        return Vec::new();
    }

    let all_apps = list_system_apps();
    let mut matches = Vec::new();

    for app in all_apps {
        let is_match = app
            .mime_types
            .iter()
            .any(|app_mime| file_mimes.iter().any(|target_mime| app_mime == target_mime));
        if is_match {
            matches.push(app);
        }
    }

    matches
}

/// Formats and launches the given command template with the given file paths.
/// Replaces `%f`/`%u` with the first file, `%F`/`%U` with all files, and strips flags.
pub fn build_launch_command(exec_template: &str, files: &[PathBuf], terminal: bool) -> Command {
    let mut tokens: Vec<String> = Vec::new();
    let mut in_quotes = false;
    let mut quote_char = ' ';
    let mut current = String::new();

    for ch in exec_template.chars() {
        if in_quotes {
            if ch == quote_char {
                in_quotes = false;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quotes = true;
            quote_char = ch;
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    let mut args: Vec<String> = Vec::new();
    let mut substituted = false;

    let file_strs: Vec<String> = files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let first_file = file_strs.first().cloned().unwrap_or_default();

    for token in tokens {
        match token.as_str() {
            "%f" | "%u" => {
                substituted = true;
                if !first_file.is_empty() {
                    args.push(first_file.clone());
                }
            }
            "%F" | "%U" => {
                substituted = true;
                for f in &file_strs {
                    args.push(f.clone());
                }
            }
            // Strip desktop exec flags
            "%i" | "%c" | "%k" | "%m" | "%v" => {}
            other => {
                if other.contains("%f") || other.contains("%u") {
                    substituted = true;
                    args.push(other.replace("%f", &first_file).replace("%u", &first_file));
                } else if other.contains("%F") || other.contains("%U") {
                    substituted = true;
                    let joined = file_strs.join(" ");
                    args.push(other.replace("%F", &joined).replace("%U", &joined));
                } else {
                    args.push(other.to_string());
                }
            }
        }
    }

    if !substituted {
        for f in file_strs {
            args.push(f);
        }
    }

    let program = if args.is_empty() {
        "sh".to_string()
    } else {
        args.remove(0)
    };

    if terminal {
        let term = std::env::var("TERMINAL").unwrap_or_else(|_| "x-terminal-emulator".to_string());
        let mut cmd = Command::new(term);
        cmd.arg("-e").arg(&program).args(args);
        cmd
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }
}

/// Spawns the desktop app detached in background.
pub fn launch_desktop_app(app: &DesktopApp, files: &[PathBuf]) -> std::io::Result<()> {
    let mut cmd = build_launch_command(&app.exec, files, app.terminal);
    cmd.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_desktop_entry() {
        let sample = r#"
[Desktop Entry]
Type=Application
Name=Audacity Audio Editor
Name[es]=Editor de audio Audacity
Exec=audacity %F
Icon=audacity
MimeType=audio/mpeg;audio/flac;audio/x-wav;
Terminal=false
NoDisplay=false
"#;
        let app = DesktopApp::parse_str("audacity.desktop", sample).expect("failed to parse");
        assert_eq!(app.id, "audacity.desktop");
        assert!(app.name.contains("Audacity"));
        assert_eq!(app.exec, "audacity %F");
        assert_eq!(app.icon.as_deref(), Some("audacity"));
        assert!(app.mime_types.contains(&"audio/mpeg".to_string()));
        assert!(!app.terminal);
    }

    #[test]
    fn test_skip_non_application_or_hidden() {
        let sample_hidden = r#"
[Desktop Entry]
Type=Application
Name=Hidden App
Exec=hidden
NoDisplay=true
"#;
        assert!(DesktopApp::parse_str("hidden.desktop", sample_hidden).is_none());

        let sample_service = r#"
[Desktop Entry]
Type=Service
Name=Service
Exec=service
"#;
        assert!(DesktopApp::parse_str("service.desktop", sample_service).is_none());
    }

    #[test]
    fn test_build_launch_command() {
        let files = vec![
            PathBuf::from("/home/user/song.mp3"),
            PathBuf::from("/home/user/song2.mp3"),
        ];

        let cmd = build_launch_command("audacity %F", &files, false);
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();

        assert_eq!(program, "audacity");
        assert_eq!(args, vec!["/home/user/song.mp3", "/home/user/song2.mp3"]);
    }

    #[test]
    fn test_mime_lookup() {
        let mimes = ext_to_mime_types("png");
        assert!(mimes.contains(&"image/png"));

        let mimes_rs = ext_to_mime_types("rs");
        assert!(mimes_rs.contains(&"text/plain") || mimes_rs.contains(&"text/rust"));
    }
}
