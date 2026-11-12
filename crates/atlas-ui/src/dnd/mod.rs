//! Drag-and-drop neutral layer.
//!
//! All platform-specific implementations (Windows OLE, future macOS/Linux) live in
//! sub-modules and expose their functionality through the types defined here. Business
//! logic (move vs copy rules, hit-testing, action dispatch) lives exclusively in this
//! module — no `#[cfg(target_os)]` in the callers.
//!
//! # Design
//! ```text
//! dnd/mod.rs            — DropEffect, DropZones (neutral)
//! dnd/windows/mod.rs    — start_drag_real(), setup_dnd()
//! dnd/macos.rs          — (future) NSPasteboard / NSDraggingSource
//! dnd/linux.rs          — (future) XDND / wl_data_device
//! ```

use std::path::PathBuf;

// Platform implementation.
#[cfg(target_os = "windows")]
pub mod windows;

pub mod linux {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub const MIME_TEXT_URI_LIST: &str = super::MIME_TEXT_URI_LIST;

    /// Resolved drop payload for Linux drop operations.
    #[derive(Debug, Clone)]
    pub struct LinuxDropPayload {
        pub paths: Vec<PathBuf>,
        pub from_self: bool,
        pub ctrl: bool,
        pub shift: bool,
        pub x: f32,
        pub y: f32,
    }

    /// Events emitted by Linux drop targets (XDND / Wayland).
    #[derive(Debug, Clone)]
    pub enum LinuxDndEvent {
        DragEnter {
            x: f32,
            y: f32,
            mime_types: Vec<String>,
        },
        DragOver {
            x: f32,
            y: f32,
        },
        DragLeave,
        Drop(LinuxDropPayload),
        Error(String),
    }

    /// Shared queue type between Linux drop hooks and the UI drain loop.
    pub type LinuxDndQueue = Arc<Mutex<Vec<LinuxDndEvent>>>;

    /// Drag-and-drop state on Linux.
    #[derive(Clone, Default)]
    pub struct LinuxDndState {
        pub zones: Arc<Mutex<DropZones>>,
        pub action_queue: LinuxDndQueue,
    }

    impl LinuxDndState {
        pub fn new() -> Self {
            Self {
                zones: Arc::new(Mutex::new(DropZones::default())),
                action_queue: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// Safely processes raw drop bytes with MIME type `text/uri-list`.
        /// Never panics on empty or malformed input.
        pub fn handle_drop_uri_list(
            &self,
            data: &[u8],
            x: f32,
            y: f32,
            ctrl: bool,
            shift: bool,
        ) -> Vec<PathBuf> {
            let paths = parse_uri_list_bytes(data);
            if !paths.is_empty() {
                if let Some(mut q) = self
                    .action_queue
                    .lock()
                    .or_else(atlas_model::sync::recover)
                    .ok()
                {
                    q.push(LinuxDndEvent::Drop(LinuxDropPayload {
                        paths: paths.clone(),
                        from_self: false,
                        ctrl,
                        shift,
                        x,
                        y,
                    }));
                }
            }
            paths
        }

        /// Checks whether any of the supplied MIME types is compatible with `text/uri-list`.
        pub fn accepts_mime_types(&self, mimes: &[String]) -> bool {
            mimes
                .iter()
                .any(|m| m == MIME_TEXT_URI_LIST || m == "text/plain")
        }
    }

    /// Safe placeholder for initializing Linux drag-and-drop.
    /// Currently returns `None`.
    pub fn init(_handle: isize, _scale_factor: f32) -> Option<LinuxDndState> {
        None
    }
}

/// Initialize the drag-and-drop subsystem for the current platform.
#[cfg(target_os = "windows")]
pub fn init(hwnd_raw: isize, scale_factor: f32) -> Option<windows::DndState> {
    windows::setup_dnd(hwnd_raw, scale_factor)
}

/// Initialize the drag-and-drop subsystem on non-Windows platforms.
/// Returns `None` placeholder.
#[cfg(not(target_os = "windows"))]
pub fn init(handle: isize, scale_factor: f32) -> Option<linux::LinuxDndState> {
    linux::init(handle, scale_factor)
}

// ─── FreeDesktop text/uri-list formatting and parsing ───────────────────────

/// Standard MIME type for FreeDesktop file transfers (both DnD and clipboard).
pub const MIME_TEXT_URI_LIST: &str = "text/uri-list";

/// Percent-decodes a URI string (e.g. `%20` -> `' '`, `%C3%A9` -> `'é'`).
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = bytes[i + 1];
            let h2 = bytes[i + 2];
            let val1 = match h1 {
                b'0'..=b'9' => Some(h1 - b'0'),
                b'a'..=b'f' => Some(h1 - b'a' + 10),
                b'A'..=b'F' => Some(h1 - b'A' + 10),
                _ => None,
            };
            let val2 = match h2 {
                b'0'..=b'9' => Some(h2 - b'0'),
                b'a'..=b'f' => Some(h2 - b'a' + 10),
                b'A'..=b'F' => Some(h2 - b'A' + 10),
                _ => None,
            };
            if let (Some(v1), Some(v2)) = (val1, val2) {
                decoded.push((v1 << 4) | v2);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Encodes a local filesystem path as a `file://` URI with percent-encoding for spaces and special characters.
pub fn path_to_file_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for &b in s.as_bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'~' => {
                out.push(b as char);
            }
            b' ' => {
                out.push_str("%20");
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Formats a list of paths as a FreeDesktop `text/uri-list` string (`\r\n` delimited).
pub fn format_uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for path in paths {
        out.push_str(&path_to_file_uri(path));
        out.push_str("\r\n");
    }
    out
}

/// Formats a list of paths as a FreeDesktop file clipboard string with cut/copy header.
/// Supported by GNOME/Nautilus, KDE/Dolphin, Thunar, PCManFM.
pub fn format_uri_list_with_cut(paths: &[PathBuf], is_cut: bool) -> String {
    let mut out = String::new();
    if is_cut {
        out.push_str("cut\r\n");
    } else {
        out.push_str("copy\r\n");
    }
    for path in paths {
        out.push_str(&path_to_file_uri(path));
        out.push_str("\r\n");
    }
    out
}

/// Parses a `text/uri-list` or multi-line text into a list of valid local `PathBuf`s and a cut flag.
/// Safely ignores comment lines (`#`), empty lines, and non-file schemes.
/// Decodes percent-encoded characters like `%20`.
pub fn parse_uri_list_with_cut(text: &str) -> (Vec<PathBuf>, bool) {
    let mut paths = Vec::new();
    let mut is_cut = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("cut") {
            is_cut = true;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("copy") {
            continue;
        }

        let uri_path = if let Some(rest) = trimmed.strip_prefix("file://localhost/") {
            Some(format!("/{rest}"))
        } else if let Some(rest) = trimmed.strip_prefix("file:///") {
            Some(format!("/{rest}"))
        } else if let Some(rest) = trimmed.strip_prefix("file://") {
            Some(if rest.starts_with('/') {
                rest.to_string()
            } else {
                format!("/{rest}")
            })
        } else if let Some(rest) = trimmed.strip_prefix("file:/") {
            Some(if rest.starts_with('/') {
                rest.to_string()
            } else {
                format!("/{rest}")
            })
        } else {
            None
        };

        if let Some(raw_path) = uri_path {
            let clean_path = if raw_path.len() >= 3
                && raw_path.starts_with('/')
                && raw_path.as_bytes()[2] == b':'
            {
                &raw_path[1..]
            } else {
                &raw_path
            };
            let decoded_str = percent_decode(clean_path);
            let path = PathBuf::from(decoded_str);
            if !path.as_os_str().is_empty() {
                paths.push(path);
            }
        } else if trimmed.starts_with('/') {
            let path = PathBuf::from(trimmed);
            if path.exists() {
                paths.push(path);
            }
        } else if cfg!(windows) && trimmed.len() >= 2 && trimmed.as_bytes().get(1) == Some(&b':') {
            let path = PathBuf::from(trimmed);
            if path.exists() {
                paths.push(path);
            }
        }
    }

    (paths, is_cut)
}

/// Parses a `text/uri-list` string into local `PathBuf`s.
pub fn parse_file_uris(text: &str) -> Vec<PathBuf> {
    parse_uri_list_with_cut(text).0
}

/// Parses raw bytes conforming to `text/uri-list` into local `PathBuf`s.
pub fn parse_uri_list_bytes(data: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(data);
    parse_file_uris(&text)
}

// ─── Core types ─────────────────────────────────────────────────────────────

/// Default outcome of a drop, decided by volume rules (same drive = move).
/// Platform-neutral so callers need not import Win32 `DROPEFFECT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEffect {
    Copy,
    Move,
}

impl DropEffect {
    /// Returns `true` when source files should be removed after the drop
    /// (i.e., the operation was effectively a move).
    pub fn is_move(self) -> bool {
        matches!(self, DropEffect::Move)
    }
}

// ─── Drop-zone registry (used by the platform drop-target) ──────────────────

/// A single region in window-space coordinates that can accept a drop, paired
/// with the destination directory and the pane that owns it.
#[derive(Clone)]
pub struct DropZone {
    /// Window-space bounding box (origin = top-left of the window client area).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Destination directory for dropped files.
    pub dest: PathBuf,
    /// Numeric pane identifier so the drop target can route correctly.
    pub pane_id: u64,
}

impl DropZone {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Per-frame registry of all visible drop zones, shared between the gpui render
/// path (writer) and the platform drop-target vtable (reader).
///
/// The render path calls [`DropZones::update`] on each frame only while a drag
/// is active (gated by [`DropZones::drag_active`]) to avoid work in the common
/// case. The platform drop-target calls [`DropZones::hit`] from the COM vtable.
#[derive(Default)]
pub struct DropZones {
    /// Set to `true` when the OS notifies us of an incoming drag (DragEnter).
    pub drag_active: bool,
    /// Last drag cursor in logical window coordinates. The OLE modal loop can
    /// starve the async queue drain, while the paint canvas still ticks.
    pub cursor: Option<(f32, f32)>,
    /// Real-file payload cached once at DragEnter; never materialize virtual
    /// files or touch the filesystem from the animation loop.
    pub paths: Vec<PathBuf>,
    pub ctrl: bool,
    pub shift: bool,
    /// Same ordered hover targets and live tab destinations used by drop routing.
    #[cfg(windows)]
    pub(crate) hover_snapshot: Option<crate::ui::drag_hover::NativeDropSnapshot>,
    zones: Vec<DropZone>,
}

impl DropZones {
    /// Replace all zones for the current frame. Called from the render path.
    pub fn update(&mut self, zones: Vec<DropZone>) {
        self.zones = zones;
    }

    /// Rewrite the destination of every zone owned by `pane_id`. Called when a
    /// pane navigates while an OLE drag is in flight: the committed zones still
    /// carry the pre-navigation directory, and the nested `DoDragDrop` loop can
    /// resolve a drop before the next frame recommits fresh zones.
    pub fn set_dest_for_pane(&mut self, pane_id: u64, dest: PathBuf) {
        for zone in &mut self.zones {
            if zone.pane_id == pane_id {
                zone.dest.clone_from(&dest);
            }
        }
    }

    /// Drop all zones owned by `pane_id` (pane navigated to a location with no
    /// filesystem directory — tag/recycle/remote — so it can't accept OLE drops).
    pub fn remove_pane(&mut self, pane_id: u64) {
        self.zones.retain(|zone| zone.pane_id != pane_id);
    }

    pub fn len(&self) -> usize {
        self.zones.len()
    }

    /// Find the deepest (last) zone that contains the given window-space point.
    /// Returns `None` when no zone matches.
    pub fn hit(&self, px: f32, py: f32) -> Option<&DropZone> {
        self.zones.iter().rev().find(|z| z.contains(px, py))
    }
}

// ─── Move-vs-copy heuristic (cross-platform) ────────────────────────────────

/// Returns the drop effect to use when no modifier keys are held, following
/// Windows Explorer semantics: same volume → move, different volume → copy.
///
/// On platforms without volume concepts (SFTP, cloud), returns `Copy`.
pub fn default_effect_for_paths(sources: &[PathBuf], dest: &PathBuf) -> DropEffect {
    let all_same = sources
        .iter()
        .all(|s| atlas_core::fs::paths::same_volume(s, dest));
    if all_same {
        DropEffect::Move
    } else {
        DropEffect::Copy
    }
}

/// Applies modifier-key overrides (Ctrl → Copy, Shift → Move) on top of the
/// default effect. `ctrl` and `shift` correspond to the key states at drop time.
/// Used by platform drop-target implementations; also exported for tests.
#[allow(dead_code)]
pub fn effect_with_modifiers(default: DropEffect, ctrl: bool, shift: bool) -> DropEffect {
    if ctrl {
        DropEffect::Copy
    } else if shift {
        DropEffect::Move
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_same_distro_moves_by_default() {
        assert_eq!(
            default_effect_for_paths(
                &[PathBuf::from(r"\\wsl$\Ubuntu\home\a")],
                &PathBuf::from(r"\\wsl.localhost\Ubuntu\tmp")
            ),
            DropEffect::Move
        );
    }

    #[test]
    fn wsl_different_distro_copies_by_default() {
        assert_eq!(
            default_effect_for_paths(
                &[PathBuf::from(r"\\wsl$\Ubuntu\home\a")],
                &PathBuf::from(r"\\wsl.localhost\Debian\home")
            ),
            DropEffect::Copy
        );
    }

    #[test]
    fn format_and_parse_uri_list_roundtrip() {
        let paths = vec![
            PathBuf::from("/home/usuario/documento.txt"),
            PathBuf::from("/home/usuario/foto.png"),
            PathBuf::from("/home/usuario/espacio en nombre.txt"),
            PathBuf::from("/home/usuario/música.mp3"),
        ];
        let uri_list = format_uri_list(&paths);
        assert!(uri_list.contains("file:///home/usuario/documento.txt\r\n"));
        assert!(uri_list.contains("file:///home/usuario/foto.png\r\n"));
        assert!(uri_list.contains("espacio%20en%20nombre.txt"));

        let parsed = parse_file_uris(&uri_list);
        assert_eq!(parsed, paths);
    }

    #[test]
    fn parse_uri_list_with_cut_and_comments() {
        let text = "# GNOME clipboard\r\ncut\r\nfile:///home/usuario/cut_file.txt\r\nfile://localhost/home/usuario/second.txt\r\n";
        let (paths, is_cut) = parse_uri_list_with_cut(text);
        assert!(is_cut);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/usuario/cut_file.txt"),
                PathBuf::from("/home/usuario/second.txt")
            ]
        );
    }

    #[test]
    fn linux_dnd_state_handles_drop_uri_list() {
        let state = linux::LinuxDndState::new();
        let data = b"file:///tmp/dropped%20file.txt\r\n";
        let paths = state.handle_drop_uri_list(data, 100.0, 200.0, false, false);
        assert_eq!(paths, vec![PathBuf::from("/tmp/dropped file.txt")]);

        let queue = state
            .action_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(queue.len(), 1);
        if let linux::LinuxDndEvent::Drop(payload) = &queue[0] {
            assert_eq!(payload.paths, paths);
            assert_eq!(payload.x, 100.0);
            assert_eq!(payload.y, 200.0);
        } else {
            panic!("expected Drop event");
        }
    }

    #[test]
    fn parse_uri_list_with_raw_existing_paths() {
        // /tmp or /etc always exist on Linux
        let existing = if cfg!(windows) { "C:\\Windows" } else { "/tmp" };
        let (paths, is_cut) = parse_uri_list_with_cut(existing);
        assert!(!is_cut);
        assert_eq!(paths, vec![PathBuf::from(existing)]);
    }
}
