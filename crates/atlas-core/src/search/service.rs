//! Versioned IPC contract and the Atlas Search Service client backend.

use super::{SearchBackend, SearchBackendStatus, SearchBatch, SearchProgress, SearchQuery};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const PIPE_PATH: &str = r"\\.\pipe\AtlasSearchService-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServiceRequest {
    Hello { protocol: u32 },
    Search(SearchQuery),
    Status,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServiceResponse {
    Hello { protocol: u32, schema: u32 },
    Search(SearchBatch),
    Status(Vec<SearchProgress>),
    Error(String),
}

pub struct WindowsServiceBackend {
    id: String,
}

impl WindowsServiceBackend {
    pub fn new() -> Self {
        Self {
            id: "atlas-search-service".into(),
        }
    }

    #[cfg(windows)]
    pub fn request(&self, request: &ServiceRequest) -> std::io::Result<ServiceResponse> {
        use std::ffi::OsStr;
        let payload = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        let response =
            atlas_win_space::call_named_pipe(OsStr::new(PIPE_PATH), &payload, 512 * 1024)?;
        serde_json::from_slice(&response).map_err(std::io::Error::other)
    }

    #[cfg(unix)]
    pub fn request(&self, request: &ServiceRequest) -> std::io::Result<ServiceResponse> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let sock_path = linux_socket_path();
        let mut stream = UnixStream::connect(sock_path)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;

        let mut payload = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        payload.push(b'\n');
        stream.write_all(&payload)?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        serde_json::from_str(line.trim()).map_err(std::io::Error::other)
    }

    #[cfg(not(any(windows, unix)))]
    pub fn request(&self, _request: &ServiceRequest) -> std::io::Result<ServiceResponse> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Search service unavailable",
        ))
    }
}

impl Default for WindowsServiceBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchBackend for WindowsServiceBackend {
    fn id(&self) -> &str {
        &self.id
    }
    fn status(&self) -> SearchProgress {
        match self.request(&ServiceRequest::Status) {
            Ok(ServiceResponse::Status(statuses)) => {
                statuses.into_iter().next().unwrap_or(SearchProgress {
                    source: self.id.clone(),
                    status: SearchBackendStatus::Loading,
                    indexed: 0,
                    total: None,
                })
            }
            Ok(ServiceResponse::Error(error)) => SearchProgress {
                source: self.id.clone(),
                status: SearchBackendStatus::Error(error),
                indexed: 0,
                total: None,
            },
            _ => SearchProgress {
                source: self.id.clone(),
                status: SearchBackendStatus::Fallback,
                indexed: 0,
                total: None,
            },
        }
    }
    fn search(&self, query: &SearchQuery) -> SearchBatch {
        match self.request(&ServiceRequest::Search(query.clone())) {
            Ok(ServiceResponse::Search(batch)) => batch,
            _ => SearchBatch {
                generation: query.generation,
                results: Vec::new(),
                complete: true,
            },
        }
    }
}

#[cfg(unix)]
pub fn linux_socket_path() -> std::path::PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        std::path::PathBuf::from(runtime_dir).join("atlas-search.sock")
    } else {
        let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
        std::path::PathBuf::from(format!("/tmp/atlas-search-{user}.sock"))
    }
}

/// Check if the search service is currently reachable and responding.
pub fn is_service_available() -> bool {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        let payload = match serde_json::to_vec(&ServiceRequest::Status) {
            Ok(p) => p,
            Err(_) => return false,
        };
        match atlas_win_space::call_named_pipe(OsStr::new(PIPE_PATH), &payload, 16 * 1024) {
            Ok(bytes) => serde_json::from_slice::<ServiceResponse>(&bytes).is_ok(),
            Err(_) => false,
        }
    }
    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let sock_path = linux_socket_path();
        if !sock_path.exists() {
            return false;
        }
        if let Ok(mut stream) = UnixStream::connect(sock_path) {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(300)));
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(300)));
            let mut payload = match serde_json::to_vec(&ServiceRequest::Status) {
                Ok(p) => p,
                Err(_) => return false,
            };
            payload.push(b'\n');
            if stream.write_all(&payload).is_err() || stream.flush().is_err() {
                return false;
            }
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                return serde_json::from_str::<ServiceResponse>(line.trim()).is_ok();
            }
        }
        false
    }
    #[cfg(not(any(windows, unix)))]
    false
}

/// Ensure the search service is running:
/// 1. If already responding, returns `true`.
/// 2. Windows: starts Windows Service or spawns `atlas-search-service.exe --console`.
/// 3. Linux: spawns `atlas-search-service --console` in background.
pub fn ensure_service_started() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        if is_service_available() {
            return true;
        }

        // Try starting the installed Windows service first
        let _ = std::process::Command::new("sc.exe")
            .args(["start", "AtlasSearchService"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if is_service_available() {
                return true;
            }
        }

        // If not running, find the binary and spawn it with --console in the background
        if let Some(exe_path) = find_search_service_exe() {
            let _ = std::process::Command::new(exe_path)
                .arg("--console")
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();

            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if is_service_available() {
                    return true;
                }
            }
        }

        is_service_available()
    }
    #[cfg(unix)]
    {
        if is_service_available() {
            return true;
        }

        if let Some(exe_path) = find_search_service_exe() {
            let _ = std::process::Command::new(exe_path)
                .arg("--console")
                .spawn();

            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if is_service_available() {
                    return true;
                }
            }
        }

        is_service_available()
    }
    #[cfg(not(any(windows, unix)))]
    false
}

/// Locate the `atlas-search-service` binary.
pub fn find_search_service_exe() -> Option<std::path::PathBuf> {
    let bin_name = if cfg!(windows) {
        "atlas-search-service.exe"
    } else {
        "atlas-search-service"
    };

    let current = std::env::current_exe().ok()?;
    let own_dir = current.parent()?;
    let packaged = own_dir.join(bin_name);
    if packaged.is_file() {
        return Some(packaged);
    }
    // Release builds only run the service shipped next to Atlas. Walking up
    // parent folders or trying CWD-relative paths would let anyone who can
    // write there (`C:\release\`, a downloaded folder) plant an executable.
    if !cfg!(debug_assertions) {
        return None;
    }
    {
        let mut cur = Some(own_dir);
        for _ in 0..4 {
            if let Some(dir) = cur {
                let direct = dir.join(bin_name);
                if direct.exists() {
                    return Some(direct);
                }
                let in_release = dir.join("release").join(bin_name);
                if in_release.exists() {
                    return Some(in_release);
                }
                let in_debug = dir.join("debug").join(bin_name);
                if in_debug.exists() {
                    return Some(in_debug);
                }
                cur = dir.parent();
            } else {
                break;
            }
        }
    }
    // Development checkouts only (`cargo run` from the repository root).
    let candidates = [
        format!("target/release/{bin_name}"),
        format!("target/debug/{bin_name}"),
    ];
    for candidate in &candidates {
        let path = std::path::Path::new(candidate);
        if path.exists() {
            return Some(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        }
    }
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]

    #[ignore = "needs a built atlas-search-service executable"]

    fn test_find_search_service_exe() {
        let exe = find_search_service_exe();
        assert!(exe.is_some(), "should locate atlas-search-service.exe in workspace target");
    }

    #[test]

    #[ignore = "needs the search service and real drives"]

    fn test_ensure_service_and_multi_drive_search() {
        let started = ensure_service_started();
        assert!(started, "search service should start and be available");
        assert!(is_service_available(), "pipe should be responsive");

        let backend = WindowsServiceBackend::new();
        let query = SearchQuery::text("ghidra");
        let batch = backend.search(&query);
        assert!(!batch.results.is_empty(), "search for 'ghidra' should return results");

        let drive_letters: std::collections::HashSet<char> = batch
            .results
            .iter()
            .filter_map(|r| r.path.to_str()?.chars().next())
            .collect();
        println!("Drives found in search results: {:?}", drive_letters);
        assert!(drive_letters.contains(&'C') || drive_letters.contains(&'F') || drive_letters.contains(&'E'));
    }
}
