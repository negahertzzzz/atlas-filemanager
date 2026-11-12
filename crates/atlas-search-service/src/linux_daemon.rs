//! Linux Search Daemon using Unix Domain Sockets and In-Memory Compact ArenaTree.

use crate::arena_index::ArenaTree;
use anyhow::Context as _;
use atlas_core::search::catalog::{SearchBackendStatus, SearchBatch, SearchProgress};
use atlas_core::search::service::{ServiceRequest, ServiceResponse, PROTOCOL_VERSION};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// Determines the canonical Unix domain socket path for Atlas search service.
pub fn linux_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("atlas-search.sock")
    } else {
        let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
        PathBuf::from(format!("/tmp/atlas-search-{user}.sock"))
    }
}

/// Directories that must never be indexed (virtual / kernel filesystems).
const EXCLUDED_DIRS: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/tmp",
    "/var/tmp",
    "/var/run",
    "/var/lock",
];

/// Subdirectory names to skip to prevent bloating the index.
const SKIPPED_SUBDIRS: &[&str] = &[
    ".git/objects",
    "node_modules",
    ".cache",
    "target/debug",
    "target/release",
];

pub fn run_daemon(foreground: bool) -> anyhow::Result<()> {
    let sock_path = linux_socket_path();
    tracing::info!(socket = %sock_path.display(), foreground, "starting atlas search daemon");

    // Clean up stale socket if present
    if sock_path.exists() {
        if UnixStream::connect(&sock_path).is_ok() {
            tracing::warn!("atlas search daemon is already running on socket");
            if !foreground {
                return Ok(());
            }
        }
        let _ = fs::remove_file(&sock_path);
    }

    let tree = Arc::new(RwLock::new(ArenaTree::new()));
    let indexing_complete = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));

    // Spawn background crawler thread
    {
        let tree_clone = Arc::clone(&tree);
        let indexing_complete_clone = Arc::clone(&indexing_complete);
        let running_clone = Arc::clone(&running);

        thread::spawn(move || {
            let start = Instant::now();
            let mut roots = Vec::new();

            if let Some(home) = dirs::home_dir() {
                roots.push(home);
            }
            // Root filesystem
            roots.push(PathBuf::from("/"));

            for root in roots {
                if !running_clone.load(Ordering::Relaxed) {
                    break;
                }
                crawl_root(&root, &tree_clone, &running_clone);
            }

            indexing_complete_clone.store(true, Ordering::Relaxed);
            let count = tree_clone.read().map(|t| t.active_count).unwrap_or(0);
            let mem_mb = tree_clone
                .read()
                .map(|t| t.memory_usage_bytes())
                .unwrap_or(0) as f64
                / 1_048_576.0;
            tracing::info!(
                indexed_files = count,
                elapsed_ms = start.elapsed().as_millis(),
                memory_mb = format!("{mem_mb:.2}"),
                "crawler finished initial index"
            );
        });
    }

    // Bind Unix Domain Socket
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("failed to bind unix socket: {}", sock_path.display()))?;

    // Restrict permissions to owner-only (0600) for security (SEC-01)
    let _ = fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600));

    listener.set_nonblocking(true)?;

    tracing::info!("listening on unix socket: {}", sock_path.display());

    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let tree_clone = Arc::clone(&tree);
                let indexing_complete_clone = Arc::clone(&indexing_complete);

                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, tree_clone, indexing_complete_clone) {
                        tracing::debug!(?err, "client disconnected");
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(15));
            }
            Err(e) => {
                tracing::error!(?e, "error accepting unix socket connection");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let _ = fs::remove_file(&sock_path);
    Ok(())
}

fn crawl_root(root: &Path, tree: &Arc<RwLock<ArenaTree>>, running: &Arc<AtomicBool>) {
    if !root.exists() {
        return;
    }

    // Register root
    let root_id = match tree.write() {
        Ok(mut t) => t.insert_root(root),
        Err(_) => return,
    };

    let walker = walkdir::WalkDir::new(root)
        .min_depth(1)
        .max_depth(12)
        .follow_links(false)
        .into_iter();

    for entry in walker.filter_entry(|e| should_index_entry(e.path())) {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        let is_dir = entry.file_type().is_dir();

        let size = if is_dir {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };

        let modified = entry.metadata().ok().and_then(|m| {
            m.modified().ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            })
        });

        // Find or create parent
        if let Ok(mut t) = tree.write() {
            let parent_id = if let Some(parent) = path.parent() {
                t.dir_paths.get(parent).copied().unwrap_or(root_id)
            } else {
                root_id
            };

            let id = t.insert(parent_id, &name, is_dir, size, modified);
            if is_dir {
                t.dir_paths.insert(path.to_path_buf(), id);
            }
        }
    }
}

fn should_index_entry(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    for excluded in EXCLUDED_DIRS {
        if path_str.starts_with(excluded) {
            return false;
        }
    }
    for skipped in SKIPPED_SUBDIRS {
        if path_str.contains(skipped) {
            return false;
        }
    }
    true
}

fn handle_client(
    mut stream: UnixStream,
    tree: Arc<RwLock<ArenaTree>>,
    indexing_complete: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();

    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        let request: ServiceRequest = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid request json: {trimmed}"))?;

        let response = match request {
            ServiceRequest::Hello { protocol: _ } => ServiceResponse::Hello {
                protocol: PROTOCOL_VERSION,
                schema: 1,
            },
            ServiceRequest::Status => {
                let (active, is_ready) = match tree.read() {
                    Ok(t) => (
                        t.active_count as u64,
                        indexing_complete.load(Ordering::Relaxed),
                    ),
                    Err(_) => (0, false),
                };
                let status = if is_ready {
                    SearchBackendStatus::Ready
                } else {
                    SearchBackendStatus::Loading
                };
                ServiceResponse::Status(vec![SearchProgress {
                    source: "atlas-linux-daemon".into(),
                    status,
                    indexed: active,
                    total: None,
                }])
            }
            ServiceRequest::Search(query) => {
                let results = match tree.read() {
                    Ok(t) => t.search(&query),
                    Err(_) => Vec::new(),
                };
                ServiceResponse::Search(SearchBatch {
                    generation: query.generation,
                    results,
                    complete: true,
                })
            }
        };

        let mut payload = serde_json::to_vec(&response)?;
        payload.push(b'\n');
        stream.write_all(&payload)?;
        stream.flush()?;

        line.clear();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::search::catalog::SearchQuery;

    #[test]
    fn test_daemon_unix_socket_roundtrip() {
        let temp_dir = std::env::temp_dir();
        let sock_path = temp_dir.join(format!("atlas_test_{}.sock", std::process::id()));
        if sock_path.exists() {
            let _ = fs::remove_file(&sock_path);
        }

        let listener = UnixListener::bind(&sock_path).expect("failed to bind test socket");
        let tree = Arc::new(RwLock::new(ArenaTree::new()));
        {
            let mut t = tree.write().expect("lock failed");
            let root = t.insert_root(Path::new("/home/testuser"));
            t.insert(root, "cargo.toml", false, 120, None);
            t.insert(root, "readme.md", false, 300, None);
        }
        let indexing_complete = Arc::new(AtomicBool::new(true));

        let srv_tree = Arc::clone(&tree);
        let srv_complete = Arc::clone(&indexing_complete);
        let srv_sock = sock_path.clone();

        let srv_handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let _ = handle_client(stream, srv_tree, srv_complete);
            }
            let _ = fs::remove_file(&srv_sock);
        });

        // Client connects and queries
        let mut client = UnixStream::connect(&sock_path).expect("connect failed");
        let req = ServiceRequest::Search(SearchQuery::text("cargo"));
        let mut payload = serde_json::to_vec(&req).expect("serialize failed");
        payload.push(b'\n');
        client.write_all(&payload).expect("write failed");
        client.flush().expect("flush failed");

        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read failed");
        let resp: ServiceResponse = serde_json::from_str(line.trim()).expect("deserialize failed");

        match resp {
            ServiceResponse::Search(batch) => {
                assert_eq!(batch.results.len(), 1);
                assert_eq!(
                    batch.results[0].path,
                    PathBuf::from("/home/testuser/cargo.toml")
                );
            }
            other => panic!("unexpected response: {:?}", other),
        }

        let _ = srv_handle.join();
    }
}
