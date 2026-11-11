//! System-clipboard interop via OLE, reusing the drag-and-drop `AtlasDataObject`.
//!
//! Copy/Cut publish `CF_HDROP` + `CFSTR_PREFERREDDROPEFFECT` (+ "PNG"/`CF_DIB`
//! for a single image) with `OleSetClipboard`, so Explorer and other apps can
//! paste Atlas files. Paste reads the same formats back with `OleGetClipboard`.
//!
//! Cut semantics follow the shell protocol: when another app pastes our cut it
//! writes `Performed DropEffect` via `SetData` — the data object forwards that
//! to the App ([`BgResult::ClipboardPerformed`]) which removes the sources
//! (unoptimized move) or just invalidates the clipboard (optimized move).
#![allow(unsafe_code)]
// reason: OLE clipboard and HGLOBAL access are inherently unsafe Win32 calls.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use atlas_model::app_state::BgResult;
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    OleFlushClipboard, OleGetClipboard, OleSetClipboard, DROPEFFECT_COPY, DROPEFFECT_MOVE,
};

use super::clipboard_formats::{cf_performed_drop_effect, cf_preferred_drop_effect};
use super::data_object::{build_bytes_hglobal, AtlasDataObject, FormatEntry};
use super::drop_target::{is_atlas_source, read_cf_hdrop};

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ColumnTransfer {
    pub location: atlas_model::location::Location,
    pub selected_paths: Vec<PathBuf>,
    pub cursor_path: Option<PathBuf>,
    pub scroll_offset: [f32; 2],
}

#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Default)]
pub(crate) struct ViewFilters {
    pub types: std::collections::HashSet<String>,
    pub sizes: std::collections::HashSet<crate::ui::size_filter_menu::SizeFilter>,
    pub show_hidden: Option<bool>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SearchTransfer {
    pub active: bool,
    pub query: String,
    pub recursive: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct TabTransfer {
    version: u32,
    token: String,
    pub tab: atlas_model::pane::TabData,
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub selected_paths: Vec<PathBuf>,
    #[serde(default)]
    pub cursor_path: Option<PathBuf>,
    #[serde(default)]
    pub scroll_offset: [f32; 2],
    #[serde(default)]
    pub tree_expanded: Vec<PathBuf>,
    #[serde(default)]
    pub miller_columns: Vec<ColumnTransfer>,
    #[serde(default)]
    pub miller_focused: usize,
    #[serde(skip)]
    pub restore_initialized: bool,
    #[serde(default)]
    pub view_filters: ViewFilters,
    #[serde(default)]
    pub search: Option<SearchTransfer>,
}

impl TabTransfer {
    fn search_state(&self) -> SearchTransfer {
        let mut search = self.search.clone().unwrap_or_default();
        // Older snapshots only carried the applied fuzzy filter. Recreate its
        // input as well, so a receiving window never hides an active filter.
        if search.query.is_empty() && !self.filter.is_empty() {
            search.active = true;
            search.query.clone_from(&self.filter);
            search.recursive = false;
        }
        search
    }

    pub(crate) fn apply_runtime(&self, state: &mut atlas_model::tab::TabState) {
        state.filter.clone_from(&self.filter);
        state.set_visibility(self.tab.show_files, self.tab.show_folders);
        state.search.reset();
        let search = self.search_state();
        state.search.active = search.active;
        state.search.query = search.query;
        state.search.recursive = search.recursive;
    }

    pub(crate) fn matches_runtime_search(&self, state: &atlas_model::tab::TabState) -> bool {
        let search = self.search_state();
        state.search.active == search.active && state.search.query == search.query && state.search.recursive == search.recursive
    }

    pub(crate) fn search_results_ready(&self, state: &atlas_model::tab::TabState) -> bool {
        self.matches_runtime_search(state) && (!state.search.is_filtering() || matches!(state.search.status,
            atlas_model::tab::SearchStatus::Ready | atlas_model::tab::SearchStatus::Error))
    }

    pub(crate) fn restore_selection(&self, state: &mut atlas_model::tab::TabState) -> bool {
        if !self.search_results_ready(state) { return false; }
        let rows: Vec<_> = if self.tab.view_mode == atlas_model::settings::ViewMode::Tree {
            state.visible_tree_rows_snapshot().into_iter().map(|row| row.0.path()).collect()
        } else { state.entries.iter().map(|entry| entry.path()).collect() };
        let selected: std::collections::HashSet<_> = self.selected_paths.iter().collect();
        state.selection.selected = rows.iter().enumerate()
            .filter(|(_, path)| selected.contains(path)).map(|(i, _)| i).collect();
        state.selection.keyboard_cursor = self.cursor_path.as_ref().and_then(|path| rows.iter().position(|entry| entry == path));
        state.selection.selection_anchor = state.selection.keyboard_cursor;
        state.selection.last_clicked_index = state.selection.keyboard_cursor;
        state.selection.scroll_to_selected = false;
        for (index, column) in state.miller.columns.iter_mut().enumerate() {
            if let Some(snapshot) = self.miller_columns.get(index) {
                let selected: std::collections::HashSet<_> = snapshot.selected_paths.iter().collect();
                column.selection.selected = column.entries.iter().enumerate()
                    .filter(|(_, entry)| selected.contains(&entry.path())).map(|(i, _)| i).collect();
                column.selection.keyboard_cursor = snapshot.cursor_path.as_ref()
                    .and_then(|path| column.entries.iter().position(|entry| entry.path() == *path));
                column.selection.selection_anchor = column.selection.keyboard_cursor;
                column.selection.last_clicked_index = column.selection.keyboard_cursor;
                column.selection.scroll_to_selected = false;
            }
        }
        state.refresh_miller_visibility();
        true
    }
}

fn decode_tab_transfer(bytes: &[u8], token: &str) -> Option<TabTransfer> {
    if bytes.len() < 4 || bytes.len() > 16 * 1024 * 1024 { return None; }
    let len = u32::from_le_bytes(bytes[..4].try_into().ok()?) as usize;
    serde_json::from_slice::<TabTransfer>(bytes.get(4..4usize.checked_add(len)?)?).ok()
        .filter(|payload| payload.version == 1 && payload.token == token)
}

/// Publish a versioned OLE snapshot. A token prevents importing an unrelated tab.
fn tab_snapshot(token: &str, tab: &atlas_model::pane::TabData, runtime: Option<&atlas_model::tab::TabState>, scroll_offset: [f32; 2], miller_offsets: &[[f32; 2]]) -> TabTransfer {
    let selected_paths = runtime.map_or_else(Vec::new, |state| state.selected_entries(&tab.view_mode)
        .into_iter().map(|entry| entry.path()).collect());
    let cursor_path = runtime.and_then(|state| {
        match tab.view_mode {
            atlas_model::settings::ViewMode::Miller => state.miller.columns.get(state.miller.focused)
                .and_then(|column| column.selection.keyboard_cursor.and_then(|i| column.entries.get(i))).map(|e| e.path()),
            atlas_model::settings::ViewMode::Tree => state.selection.keyboard_cursor
                .and_then(|i| state.visible_tree_rows_snapshot().get(i).map(|row| row.0.path())),
            _ => state.selection.keyboard_cursor.and_then(|i| state.entries.get(i)).map(|e| e.path()),
        }
    });
    let miller_columns = runtime.map_or_else(Vec::new, |state| state.miller.columns.iter().enumerate().map(|(index, column)| ColumnTransfer {
        location: column.location.clone(),
        selected_paths: column.selection.selected.iter().filter_map(|i| column.entries.get(*i)).map(|e| e.path()).collect(),
        cursor_path: column.selection.keyboard_cursor.and_then(|i| column.entries.get(i)).map(|e| e.path()),
        scroll_offset: miller_offsets.get(index).copied().unwrap_or_default(),
    }).collect());
    TabTransfer { version: 1, token: token.to_owned(), tab: tab.clone(),
        filter: runtime.map_or_else(String::new, |state| state.filter.clone()), selected_paths, cursor_path, scroll_offset,
        tree_expanded: runtime.map_or_else(Vec::new, |state| state.tree.expanded.iter().cloned().collect()),
        miller_columns, miller_focused: runtime.map_or(0, |state| state.miller.focused), restore_initialized: false,
        view_filters: ViewFilters::default(), search: runtime.map(|state| SearchTransfer {
            active: state.search.active, query: state.search.query.clone(), recursive: state.search.recursive,
        }) }
}

pub fn publish_tab_transfer(token: &str, tab: &atlas_model::pane::TabData, runtime: Option<&atlas_model::tab::TabState>, scroll_offset: [f32; 2], miller_offsets: &[[f32; 2]], keep_navigation: bool, view_filters: ViewFilters) -> Result<(), String> {
    let mut snapshot = tab_snapshot(token, tab, runtime, scroll_offset, miller_offsets);
    snapshot.view_filters = view_filters;
    if !keep_navigation {
        snapshot.selected_paths.clear();
        snapshot.cursor_path = None;
        snapshot.tree_expanded.clear();
        snapshot.miller_columns.clear();
        snapshot.miller_focused = 0;
        snapshot.scroll_offset = [0.0; 2];
    }
    let payload = serde_json::to_vec(&snapshot)
        .map_err(|error| error.to_string())?;
    let mut framed = (payload.len() as u32).to_le_bytes().to_vec();
    framed.extend(payload);
    let hg = unsafe { build_bytes_hglobal(&framed) };
    if hg == 0 { return Err("Cannot allocate tab transfer".into()); }
    let paths: Vec<PathBuf> = tab.location.fs_dir().map(|path| vec![path.to_path_buf()]).unwrap_or_default();
    let object = AtlasDataObject::with_extras(&paths, DROPEFFECT_COPY, Arc::new(AtomicU32::new(0)),
        vec![FormatEntry::new_hglobal(super::clipboard_formats::cf_open_window(), hg)], None)
        .ok_or_else(|| "Cannot construct tab transfer".to_owned())?;
    unsafe { OleSetClipboard(&object).map_err(|error| error.message())?; }
    Ok(())
}

pub(crate) fn take_tab_transfer(token: &str) -> Option<TabTransfer> {
    unsafe {
        let object = OleGetClipboard().ok()?;
        let format = fmt_u32(super::clipboard_formats::cf_open_window());
        let mut medium = object.GetData(&format).ok()?;
        let handle = medium.u.hGlobal;
        let size = GlobalSize(handle);
        let ptr = GlobalLock(handle) as *const u8;
        let result = if ptr.is_null() || size < 4 || size > 16 * 1024 * 1024 {
            None
        } else {
            let bytes = std::slice::from_raw_parts(ptr, size);
            decode_tab_transfer(bytes, token)
        };
        if !ptr.is_null() { let _ = GlobalUnlock(handle); }
        windows::Win32::System::Ole::ReleaseStgMedium(&mut medium);
        result
    }
}

fn transfer_event_name(token: &str) -> Vec<u16> {
    format!("Local\\AtlasTabTransfer-{token}\0").encode_utf16().collect()
}

/// Acknowledge only after startup has applied the transferred snapshot.
pub fn acknowledge_tab_transfer(token: &str) {
    use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    let name = transfer_event_name(token);
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, windows::core::PCWSTR(name.as_ptr())) {
            let _ = SetEvent(event);
            let _ = windows::Win32::Foundation::CloseHandle(event);
        }
    }
}

/// Hold the OLE source alive until the new process confirms it imported the tab.
pub fn prepare_transfer_ack(token: &str) -> Result<isize, String> {
    use windows::Win32::System::Threading::CreateEventW;
    let name = transfer_event_name(token);
    unsafe { CreateEventW(None, true, false, windows::core::PCWSTR(name.as_ptr()))
        .map(|event| event.0 as isize).map_err(|error| error.message()) }
}

pub fn wait_transfer_ack(event: isize) -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::WaitForSingleObject;
    unsafe {
        let event = HANDLE(event as *mut _);
        let accepted = WaitForSingleObject(event, 15_000) == WAIT_OBJECT_0;
        let _ = CloseHandle(event);
        accepted
    }
}

/// Don't decode images above this size for the PNG/DIB clipboard extras —
/// `set_clipboard_files` runs on the UI thread.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

const fn fmt_u32(cf: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

unsafe fn read_u32_format(obj: &IDataObject, cf: u16) -> Option<u32> {
    use windows::Win32::System::Ole::ReleaseStgMedium;
    let fmt = fmt_u32(cf);
    let mut medium = obj.GetData(std::ptr::from_ref(&fmt)).ok()?;
    if medium.u.hGlobal.is_invalid() {
        return None;
    }
    let ptr = GlobalLock(medium.u.hGlobal) as *const u32;
    let val = if ptr.is_null() || GlobalSize(medium.u.hGlobal) < std::mem::size_of::<u32>() {
        None
    } else {
        Some(ptr.read_unaligned())
    };
    if !ptr.is_null() {
        let _ = GlobalUnlock(medium.u.hGlobal);
    }
    ReleaseStgMedium(std::ptr::from_mut(&mut medium));
    val
}

// ─── image extras ────────────────────────────────────────────────────────────

fn is_image_ext(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "ico"
        )
    })
}

/// "PNG" registered clipboard format (used by browsers, Discord, GIMP…).
fn cf_png() -> u16 {
    use std::sync::OnceLock;
    use windows::core::w;
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    static ID: OnceLock<u16> = OnceLock::new();
    *ID.get_or_init(|| unsafe { RegisterClipboardFormatW(w!("PNG")) as u16 })
}

/// Build a packed 32-bpp `CF_DIB` (BITMAPINFOHEADER + bottom-up BGRA rows).
fn build_dib(img: &image::RgbaImage) -> Vec<u8> {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let mut out = Vec::with_capacity(40 + (w * h * 4) as usize);
    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&w.to_le_bytes()); // biWidth
    out.extend_from_slice(&h.to_le_bytes()); // biHeight (positive = bottom-up)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    out.extend_from_slice(&[0u8; 16]); // x/y ppm, clr used/important
                                       // Bottom-up BGRA pixel rows.
    for row in img.rows().rev() {
        for px in row {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    out
}

/// PNG + DIB clipboard entries for a single image file, when cheap enough.
fn image_format_entries(paths: &[PathBuf]) -> Vec<FormatEntry> {
    if paths.iter().any(|path| atlas_core::fs::preview::is_cloud_placeholder(path)) {
        return Vec::new();
    }
    let [path] = paths else { return Vec::new() };
    if !is_image_ext(path)
        || !path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= MAX_IMAGE_BYTES)
    {
        return Vec::new();
    }
    #[allow(clippy::disallowed_methods)]
    // reason: ≤20 MB read on Ctrl+C of a single image; OleSetClipboard must run
    // on this (STA) thread anyway.
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(img) = image::load_from_memory(&bytes) else {
        return Vec::new();
    };
    let rgba = img.to_rgba8();
    let png_bytes = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        bytes
    } else {
        let mut buf = std::io::Cursor::new(Vec::new());
        if img.write_to(&mut buf, image::ImageFormat::Png).is_err() {
            return Vec::new();
        }
        buf.into_inner()
    };
    let dib = build_dib(&rgba);
    unsafe {
        vec![
            FormatEntry::new_hglobal(cf_png(), build_bytes_hglobal(&png_bytes)),
            FormatEntry::new_hglobal(
                windows::Win32::System::Ole::CF_DIB.0,
                build_bytes_hglobal(&dib),
            ),
        ]
    }
}

// ─── public API ──────────────────────────────────────────────────────────────

/// Publish `paths` to the system clipboard as a copy (`cut = false`) or cut.
/// `tx` receives [`BgResult::ClipboardPerformed`] when a paste target reports
/// what it did with the data.
pub fn set_clipboard_files(
    paths: &[PathBuf],
    cut: bool,
    tx: flume::Sender<BgResult>,
) -> Result<(), String> {
    let paths = atlas_core::fs::paths::normalize_wsl_unc_paths(paths.to_vec());
    let effect = if cut {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_COPY
    };
    let extras = if cut {
        Vec::new()
    } else {
        image_format_entries(&paths)
    };
    let obj = AtlasDataObject::with_extras(
        &paths,
        effect,
        Arc::new(AtomicU32::new(0)),
        extras,
        Some((tx, paths.clone())),
    )
    .ok_or_else(|| "no se pudo preparar el contenido".to_string())?;
    unsafe { OleSetClipboard(&obj) }.map_err(|e| e.message())
}

pub struct ClipboardContents {
    pub paths: Vec<PathBuf>,
    /// The source prefers a move (it was a cut).
    pub preferred_move: bool,
    /// The clipboard content was put there by Atlas itself.
    pub from_atlas: bool,
}

/// Read file paths (`CF_HDROP`) from the system clipboard, if any.
pub fn get_clipboard_files() -> Option<ClipboardContents> {
    unsafe {
        let obj = OleGetClipboard().ok()?;
        let paths = atlas_core::fs::paths::normalize_wsl_unc_paths(read_cf_hdrop(&obj));
        if paths.is_empty() {
            return None;
        }
        let preferred = read_u32_format(&obj, cf_preferred_drop_effect()).unwrap_or(0);
        Some(ClipboardContents {
            paths,
            preferred_move: preferred & DROPEFFECT_MOVE.0 != 0,
            from_atlas: is_atlas_source(&obj),
        })
    }
}

/// After successfully pasting files cut in another app: report
/// `Performed DropEffect = MOVE` back to the source data object so it clears
/// its cut state (e.g. Explorer un-ghosts and stops offering the cut).
pub fn acknowledge_external_cut() {
    unsafe {
        let Ok(obj) = OleGetClipboard() else { return };
        let hg = super::data_object::build_u32_hglobal(DROPEFFECT_MOVE.0);
        if hg == 0 {
            return;
        }
        let mut medium: windows::Win32::System::Com::STGMEDIUM = std::mem::zeroed();
        medium.tymed = TYMED_HGLOBAL.0 as u32;
        medium.u.hGlobal = windows::Win32::Foundation::HGLOBAL(hg as *mut _);
        medium.pUnkForRelease = std::mem::ManuallyDrop::new(None);
        let fmt = fmt_u32(cf_performed_drop_effect());
        // fRelease = TRUE: the data object takes ownership of the HGLOBAL.
        let _ = obj.SetData(std::ptr::from_ref(&fmt), std::ptr::from_ref(&medium), true);
        // The cut is consumed; an empty clipboard avoids a second stale paste.
        let _ = OleSetClipboard(None);
    }
}

/// Empty the system clipboard (after our own cut has been consumed).
pub fn clear() {
    unsafe {
        let _ = OleSetClipboard(None);
    }
}

/// Make the clipboard contents outlive the app (called on shutdown).
pub fn flush() {
    unsafe {
        let _ = OleFlushClipboard();
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;

    fn entry(path: &str, directory: bool) -> atlas_model::file_entry::FileEntry {
        atlas_model::file_entry::FileEntryData {
            name: Path::new(path).file_name().unwrap().to_string_lossy().as_ref().into(),
            path: path.into(), kind: if directory { atlas_model::file_entry::FileKind::Dir } else { atlas_model::file_entry::FileKind::File },
            size: None, modified: None, _is_hidden: false, _is_symlink: false,
            archive_root: None, recycle_info: None,
        }.into()
    }

    #[test]
    fn snapshot_uses_tree_rows_and_preserves_the_entire_miller_cascade() {
        use atlas_model::{settings::ViewMode, tab::{TabState, MillerColumn}, location::Location};
        let mut tab = atlas_model::pane::TabData::with_view("C:/test".into(), ViewMode::Tree, 192.0, true);
        let mut state = TabState::default();
        let parent = PathBuf::from("C:/test/dir");
        state.set_entries(vec![entry("C:/test/dir", true)].into());
        state.tree.expanded.insert(parent.clone());
        state.tree.children.insert(parent.clone(), vec![entry("C:/test/dir/child.txt", false)].into());
        state.selection.selected.insert(1);
        state.selection.keyboard_cursor = Some(1);
        state.filter = "child".into();
        // Clear the filter for the tree projection; retain it in the snapshot
        // below after establishing the actual visible-row identity.
        state.filter.clear();
        let snapshot = tab_snapshot("token", &tab, Some(&state), [0.0, -200.0], &[]);
        assert_eq!(snapshot.selected_paths, vec![PathBuf::from("C:/test/dir/child.txt")]);
        assert_eq!(snapshot.cursor_path, snapshot.selected_paths.first().cloned());
        assert_eq!(snapshot.tree_expanded, vec![parent]);
        tab.view_mode = ViewMode::Miller;
        let mut root = MillerColumn::new(Location::Directory("C:/test".into()));
        root.entries = vec![entry("C:/test/dir", true)].into();
        root.selection.selected.insert(0);
        let mut child = MillerColumn::new(Location::Directory("C:/test/dir".into()));
        child.entries = vec![entry("C:/test/dir/child.txt", false)].into();
        child.selection.selected.insert(0);
        child.selection.keyboard_cursor = Some(0);
        state.miller.columns = vec![root, child];
        state.miller.focused = 1;
        state.filter = "child".into();
        let snapshot = tab_snapshot("token", &tab, Some(&state), [0.0, -200.0], &[[0.0, -50.0], [0.0, -100.0]]);
        let decoded: TabTransfer = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert_eq!(decoded.miller_columns.len(), 2);
        assert_eq!(decoded.miller_focused, 1);
        assert_eq!(decoded.selected_paths, vec![PathBuf::from("C:/test/dir/child.txt")]);
        assert_eq!(decoded.miller_columns[0].selected_paths, vec![PathBuf::from("C:/test/dir")]);
        assert_eq!(decoded.miller_columns[1].scroll_offset, [0.0, -100.0]);
        assert_eq!(decoded.filter, "child");
    }

    #[test]
    fn framed_snapshot_keeps_runtime_state_and_rejects_wrong_or_truncated_payloads() {
        let tab = atlas_model::pane::TabData::with_view("C:/test".into(),
            atlas_model::settings::ViewMode::Details, 192.0, true);
        let transfer = TabTransfer { version: 1, token: "test-token".into(), tab,
            filter: "résumé".into(), selected_paths: vec!["C:/test/a.txt".into()],
            cursor_path: Some("C:/test/a.txt".into()), scroll_offset: [0.0, -200.0],
            tree_expanded: vec![], miller_columns: vec![], miller_focused: 0, restore_initialized: false,
            view_filters: ViewFilters { types: ["Imagen".into()].into(),
                sizes: [crate::ui::size_filter_menu::SizeFilter::Large].into(), show_hidden: Some(true) }, search: None };
        let payload = serde_json::to_vec(&transfer).unwrap();
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend(payload);
        let decoded = decode_tab_transfer(&frame, "test-token").unwrap();
        assert_eq!(decoded.filter, "résumé");
        assert_eq!(decoded.selected_paths, transfer.selected_paths);
        assert_eq!(decoded.cursor_path, transfer.cursor_path);
        assert_eq!(decoded.scroll_offset, transfer.scroll_offset);
        assert_eq!(decoded.view_filters.types, transfer.view_filters.types);
        assert_eq!(decoded.view_filters.sizes, transfer.view_filters.sizes);
        assert_eq!(decoded.view_filters.show_hidden, Some(true));
        assert!(decode_tab_transfer(&frame, "unrelated").is_none());
        assert!(decode_tab_transfer(&frame[..frame.len() - 1], "test-token").is_none());
        assert!(decode_tab_transfer(&[255; 4], "test-token").is_none());
    }

    #[test]
    fn transfer_recreates_the_search_input_and_waits_for_results_before_remapping_selection() {
        use atlas_model::{settings::ViewMode, tab::{TabState, SearchStatus}};
        let tab = atlas_model::pane::TabData::with_view("C:/test".into(), ViewMode::Details, 192.0, true);
        let mut source = TabState::default();
        source.set_entries(vec![entry("C:/test/sub/result.txt", false)].into());
        source.selection.selected.insert(0);
        source.selection.keyboard_cursor = Some(0);
        source.search.active = true;
        source.search.recursive = true;
        source.search.query = "r\u{00e9}sultat".into();
        let snapshot = tab_snapshot("token", &tab, Some(&source), [0.0, -400.0], &[]);
        let received: TabTransfer = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let mut target = TabState::default();
        received.apply_runtime(&mut target);
        assert!(target.search.active && target.search.recursive);
        assert_eq!(target.search.query, "r\u{00e9}sultat");
        target.set_entries(vec![entry("C:/test/unfiltered.txt", false)].into());
        target.selection.selected.insert(0);
        target.search.status = SearchStatus::Searching;
        assert!(!received.restore_selection(&mut target));
        assert_eq!(target.selection.selected, [0].into());
        target.set_entries(vec![entry("C:/test/other-result.txt", false), entry("C:/test/sub/result.txt", false)].into());
        target.search.status = SearchStatus::Ready;
        assert!(received.restore_selection(&mut target));
        assert_eq!(target.selection.selected, [1].into());
        assert_eq!(target.selection.keyboard_cursor, Some(1));
        assert!(!target.selection.scroll_to_selected);
        assert!(target.filter.is_empty(), "recursive results must not inherit an old fuzzy filter");
        target.search.query = "a new query".into();
        assert!(!received.matches_runtime_search(&target));
        assert!(!received.restore_selection(&mut target));
    }

    #[test]
    fn legacy_filter_only_snapshots_restore_a_visible_filter_input() {
        use atlas_model::{settings::ViewMode, tab::{TabState, SearchStatus}};
        let tab = atlas_model::pane::TabData::with_view("C:/test".into(), ViewMode::Details, 192.0, true);
        let mut source = TabState::default();
        source.filter = "*.rs".into();
        let snapshot = tab_snapshot("token", &tab, Some(&source), [0.0; 2], &[]);
        let mut legacy = serde_json::to_value(snapshot).unwrap();
        legacy.as_object_mut().unwrap().remove("search");
        let received: TabTransfer = serde_json::from_value(legacy).unwrap();
        let mut target = TabState::default();
        received.apply_runtime(&mut target);
        assert_eq!(target.filter, "*.rs");
        assert_eq!(target.search.query, "*.rs");
        assert!(target.search.active && !target.search.recursive);
        assert!(!received.search_results_ready(&target));
        target.search.status = SearchStatus::Ready;
        assert!(received.search_results_ready(&target));
        target.search.active = false;
        assert!(!received.search_results_ready(&target));
    }
}
