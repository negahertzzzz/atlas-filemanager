//! `IDataObject` implementation for Atlas drag-out.
//!
//! # Formats offered (Phase 1 — real files)
//! - `CF_HDROP`                — HGLOBAL with `DROPFILES` + UTF-16 path list
//! - `CFSTR_PREFERREDDROPEFFECT` — HGLOBAL with `DROPEFFECT` (COPY|MOVE)
//! - `AtlasInternalSource`       — HGLOBAL with single `0u32` (signals intra-Atlas)
//!
//! # Formats consumed (written by target)
//! - `CFSTR_PERFORMEDDROPEFFECT` — stored in `performed_effect`; caller reads it
//!   after `DoDragDrop` returns to decide whether to delete source files.
//!
//! # Future (Phase 5 — virtual / archive)
//! Add `CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` (TYMED_ISTREAM) here.
#![allow(unsafe_code)]
// reason: COM vtable, HGLOBAL and raw-pointer manipulation are inherently unsafe.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use windows::core::{implement, Error, Ref, Result, BOOL, HRESULT};
use windows::Win32::Foundation::{
    GlobalFree, E_INVALIDARG, E_OUTOFMEMORY, E_POINTER, HGLOBAL, S_FALSE,
};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumFORMATETC_Impl, IEnumSTATDATA,
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{ReleaseStgMedium, CF_HDROP, DROPEFFECT};
use windows::Win32::UI::Shell::DROPFILES;

use super::clipboard_formats::{
    cf_atlas_source, cf_performed_drop_effect, cf_preferred_drop_effect,
};

// ─── helpers ────────────────────────────────────────────────────────────────

/// Build an `HGLOBAL` containing a `DROPFILES` structure followed by a
/// double-null-terminated list of UTF-16 path strings.
///
/// # Safety
/// The returned `isize` is a valid HGLOBAL if non-zero.
/// The caller is responsible for calling `GlobalFree` eventually.
unsafe fn build_hdrop(paths: &[PathBuf]) -> isize {
    // Encode each path as a null-terminated UTF-16 string.
    let mut wide: Vec<u16> = Vec::new();
    for path in paths {
        wide.extend(OsStr::new(path).encode_wide());
        wide.push(0); // null terminator
    }
    wide.push(0); // double-null list terminator

    let dropfiles_size = std::mem::size_of::<DROPFILES>();
    let total = dropfiles_size + wide.len() * std::mem::size_of::<u16>();

    let hglobal = GlobalAlloc(GMEM_MOVEABLE, total).unwrap_or_default();
    if hglobal.is_invalid() {
        return 0;
    }
    let ptr = GlobalLock(hglobal) as *mut u8;
    if ptr.is_null() {
        let _ = GlobalFree(Some(hglobal));
        return 0;
    }

    // Zero the header.
    std::ptr::write_bytes(ptr, 0, total);

    let df = ptr as *mut DROPFILES;
    (*df).pFiles = dropfiles_size as u32;
    (*df).fWide = BOOL(1);

    // Copy wide chars after the DROPFILES header.
    let data_ptr = ptr.add(dropfiles_size) as *mut u16;
    for (i, &ch) in wide.iter().enumerate() {
        data_ptr.add(i).write(ch);
    }

    let _ = GlobalUnlock(hglobal);
    hglobal.0 as isize
}

/// Build an `HGLOBAL` containing arbitrary bytes (e.g. PNG data or a DIB).
pub(super) unsafe fn build_bytes_hglobal(bytes: &[u8]) -> isize {
    if bytes.is_empty() {
        return 0;
    }
    let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).unwrap_or_default();
    if hglobal.is_invalid() {
        return 0;
    }
    let ptr = GlobalLock(hglobal) as *mut u8;
    if ptr.is_null() {
        let _ = GlobalFree(Some(hglobal));
        return 0;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    let _ = GlobalUnlock(hglobal);
    hglobal.0 as isize
}

/// Build an `HGLOBAL` containing a single `u32` value (e.g. `DROPEFFECT`).
pub(super) unsafe fn build_u32_hglobal(value: u32) -> isize {
    let hglobal = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of::<u32>()).unwrap_or_default();
    if hglobal.is_invalid() {
        return 0;
    }
    let ptr = GlobalLock(hglobal) as *mut u32;
    if ptr.is_null() {
        let _ = GlobalFree(Some(hglobal));
        return 0;
    }
    ptr.write(value);
    let _ = GlobalUnlock(hglobal);
    hglobal.0 as isize
}

/// Clone an HGLOBAL by allocating a new one of the same size and `memcpy`.
unsafe fn clone_hglobal(src: isize) -> isize {
    let src_hg = HGLOBAL(src as *mut _);
    let size = GlobalSize(src_hg);
    if size == 0 {
        return 0;
    }
    let src_ptr = GlobalLock(src_hg) as *const u8;
    if src_ptr.is_null() {
        return 0;
    }
    let dst_hg = GlobalAlloc(GMEM_MOVEABLE, size).unwrap_or_default();
    if dst_hg.is_invalid() {
        let _ = GlobalUnlock(src_hg);
        return 0;
    }
    let dst_ptr = GlobalLock(dst_hg) as *mut u8;
    if dst_ptr.is_null() {
        let _ = GlobalUnlock(src_hg);
        let _ = GlobalFree(Some(dst_hg));
        return 0;
    }
    std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, size);
    let _ = GlobalUnlock(src_hg);
    let _ = GlobalUnlock(dst_hg);
    dst_hg.0 as isize
}

// ─── Format-entry cache (pre-built at construction) ─────────────────────────

/// One pre-rendered format: its descriptor + the raw HGLOBAL (or 0 if failed).
pub(super) struct FormatEntry {
    fmt: FORMATETC,
    hglobal: isize,
}

impl FormatEntry {
    pub(super) fn new_hglobal(cf: u16, hg: isize) -> Self {
        Self {
            fmt: FORMATETC {
                cfFormat: cf,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            },
            hglobal: hg,
        }
    }
}

impl Drop for FormatEntry {
    fn drop(&mut self) {
        if self.hglobal != 0 {
            unsafe {
                let _ = GlobalFree(Some(HGLOBAL(self.hglobal as *mut _)));
            }
            self.hglobal = 0;
        }
    }
}

// ─── AtlasDataObject ────────────────────────────────────────────────────────

/// Atlas `IDataObject` for drag-out.
///
/// Pre-renders all offered formats at construction so `GetData` is O(n) on the
/// number of formats (typically ≤ 3). Wraps the format list in raw indexing —
/// all access is on the UI thread (STA).
///
/// `performed_effect` is an `Arc<AtomicU32>` shared with the call site; after
/// `DoDragDrop` the target will have called `SetData(CFSTR_PERFORMEDDROPEFFECT, …)`
/// which stores the effect here.
#[implement(IDataObject)]
pub struct AtlasDataObject {
    formats: Vec<FormatEntry>,
    /// Shell drag-image helpers and targets add HGLOBAL formats during a drag.
    dynamic: std::cell::RefCell<Vec<FormatEntry>>,
    /// Receives the `CFSTR_PERFORMEDDROPEFFECT` written by the drop target.
    pub performed_effect: Arc<AtomicU32>,
    /// The format ID for `CFSTR_PERFORMEDDROPEFFECT` (registered at construction).
    cf_performed: u16,
    /// Clipboard use only: when the paste target reports a performed effect
    /// (e.g. Explorer pasting a cut), forward it to the App's drain loop so the
    /// sources can be trashed / the clipboard invalidated.
    on_performed: Option<(
        flume::Sender<atlas_model::app_state::BgResult>,
        Vec<PathBuf>,
    )>,
}

impl AtlasDataObject {
    /// Create a data object for dragging real filesystem `paths`.
    ///
    /// `allowed` is a bitmask of `DROPEFFECT_COPY | DROPEFFECT_MOVE` etc.
    /// `performed_effect` is written by the target after a drop.
    /// Returns `None` when the CF_HDROP block couldn't be built (allocation
    /// failure) — offering a broken object would fail opaquely in the consumer.
    pub fn new(
        paths: &[PathBuf],
        allowed: DROPEFFECT,
        performed_effect: Arc<AtomicU32>,
    ) -> Option<IDataObject> {
        let preferred = if allowed.0 & windows::Win32::System::Ole::DROPEFFECT_MOVE.0 != 0 {
            windows::Win32::System::Ole::DROPEFFECT_MOVE
        } else {
            windows::Win32::System::Ole::DROPEFFECT_COPY
        };
        Self::with_preferred(paths, preferred, performed_effect, Vec::new(), None)
    }

    /// Like [`Self::new`] but with extra pre-built formats (clipboard image
    /// formats) and a performed-effect notification channel (clipboard cut).
    pub(super) fn with_extras(
        paths: &[PathBuf],
        allowed: DROPEFFECT,
        performed_effect: Arc<AtomicU32>,
        extra_formats: Vec<FormatEntry>,
        on_performed: Option<(
            flume::Sender<atlas_model::app_state::BgResult>,
            Vec<PathBuf>,
        )>,
    ) -> Option<IDataObject> {
        Self::with_preferred(
            paths,
            allowed,
            performed_effect,
            extra_formats,
            on_performed,
        )
    }

    fn with_preferred(
        paths: &[PathBuf],
        preferred_effect: DROPEFFECT,
        performed_effect: Arc<AtomicU32>,
        extra_formats: Vec<FormatEntry>,
        on_performed: Option<(
            flume::Sender<atlas_model::app_state::BgResult>,
            Vec<PathBuf>,
        )>,
    ) -> Option<IDataObject> {
        let paths = atlas_core::fs::paths::normalize_wsl_unc_paths(paths.to_vec());
        let cf_performed = cf_performed_drop_effect();
        let mut formats = unsafe {
            let hdrop = build_hdrop(&paths);
            if hdrop == 0 {
                return None;
            }
            let preferred = build_u32_hglobal(preferred_effect.0);
            let atlas = build_u32_hglobal(0);
            vec![
                FormatEntry::new_hglobal(CF_HDROP.0, hdrop),
                FormatEntry::new_hglobal(cf_preferred_drop_effect(), preferred),
                FormatEntry::new_hglobal(cf_atlas_source(), atlas),
            ]
        };
        formats.extend(extra_formats);
        // Drop secondary formats whose HGLOBAL failed to build.
        formats.retain(|f| f.hglobal != 0);
        Some(
            AtlasDataObject {
                formats,
                dynamic: Default::default(),
                performed_effect,
                cf_performed,
                on_performed,
            }
            .into(),
        )
    }

    /// Find the index of the matching format entry (TYMED and aspect insensitive
    /// for QueryGetData compatibility).
    fn find(&self, pfe: &FORMATETC) -> Option<usize> {
        self.formats.iter().position(|e| {
            e.fmt.cfFormat == pfe.cfFormat
                && (pfe.dwAspect == 0 || e.fmt.dwAspect == pfe.dwAspect)
                && (pfe.lindex == -1 || e.fmt.lindex == pfe.lindex)
        })
    }
}

impl IDataObject_Impl for AtlasDataObject_Impl {
    /// Returns a clone of the requested format's HGLOBAL.
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        if pformatetcin.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        let fmt = unsafe { &*pformatetcin };
        let dynamic = self.dynamic.borrow();
        let hg = dynamic
            .iter()
            .find(|entry| entry.fmt.cfFormat == fmt.cfFormat && entry.fmt.lindex == fmt.lindex)
            .map(|entry| entry.hglobal)
            .or_else(|| self.find(fmt).map(|idx| self.formats[idx].hglobal))
            .ok_or_else(|| Error::from(DV_E_FORMATETC))?;
        if hg == 0 {
            return Err(Error::from(E_INVALIDARG));
        }
        let cloned = unsafe { clone_hglobal(hg) };
        if cloned == 0 {
            return Err(Error::from(E_OUTOFMEMORY));
        }
        let mut med: STGMEDIUM = unsafe { std::mem::zeroed() };
        med.tymed = TYMED_HGLOBAL.0 as u32;
        med.u.hGlobal = HGLOBAL(cloned as *mut _);
        med.pUnkForRelease = std::mem::ManuallyDrop::new(None);
        Ok(med)
    }

    fn GetDataHere(&self, _pformatetcin: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> Result<()> {
        Err(Error::from(DATA_E_FORMATETC))
    }

    fn QueryGetData(&self, pformatetcin: *const FORMATETC) -> HRESULT {
        if pformatetcin.is_null() {
            return E_INVALIDARG;
        }
        let fmt = unsafe { &*pformatetcin };
        if self.find(fmt).is_some()
            || self
                .dynamic
                .borrow()
                .iter()
                .any(|entry| entry.fmt.cfFormat == fmt.cfFormat && entry.fmt.lindex == fmt.lindex)
        {
            HRESULT(0) // S_OK
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatetcin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        if !pformatetcout.is_null() {
            unsafe { (*pformatetcout).ptd = std::ptr::null_mut() };
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        pformatetcin: *const FORMATETC,
        pmedium: *const STGMEDIUM,
        frelease: BOOL,
    ) -> Result<()> {
        if pformatetcin.is_null() || pmedium.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        let fmt = unsafe { &*pformatetcin };
        // Capture CFSTR_PERFORMEDDROPEFFECT so the caller can read it.
        if fmt.cfFormat == self.cf_performed {
            let med = unsafe { &*pmedium };
            if med.tymed == TYMED_HGLOBAL.0 as u32
                && unsafe { GlobalSize(med.u.hGlobal) } >= std::mem::size_of::<u32>()
            {
                unsafe {
                    let ptr = GlobalLock(med.u.hGlobal) as *const u32;
                    if !ptr.is_null() {
                        let val = ptr.read();
                        self.performed_effect.store(val, Ordering::Relaxed);
                        let _ = GlobalUnlock(med.u.hGlobal);
                        // Clipboard paste: tell the App what the paste target
                        // did with our cut/copy (move ⇒ trash sources).
                        if let Some((tx, paths)) = &self.on_performed {
                            let _ = tx.send(atlas_model::app_state::BgResult::ClipboardPerformed {
                                effect: val,
                                paths: paths.clone(),
                            });
                        }
                    }
                }
            }
            if frelease.as_bool() {
                unsafe {
                    ReleaseStgMedium(pmedium.cast_mut());
                }
            }
            return Ok(());
        }
        let medium = unsafe { &*pmedium };
        if medium.tymed != TYMED_HGLOBAL.0 as u32 {
            return Err(Error::from(E_INVALIDARG));
        }
        let size = unsafe { GlobalSize(medium.u.hGlobal) };
        if size == 0 || size > 16 * 1024 * 1024 {
            return Err(Error::from(E_INVALIDARG));
        }
        let mut dynamic = self.dynamic.borrow_mut();
        let existing = dynamic
            .iter()
            .position(|entry| entry.fmt.cfFormat == fmt.cfFormat && entry.fmt.lindex == fmt.lindex);
        if existing.is_none() && dynamic.len() >= 64 {
            return Err(Error::from(E_OUTOFMEMORY));
        }
        let copy = unsafe { clone_hglobal(medium.u.hGlobal.0 as isize) };
        if copy == 0 {
            return Err(Error::from(E_OUTOFMEMORY));
        }
        let mut entry = FormatEntry::new_hglobal(fmt.cfFormat, copy);
        entry.fmt = *fmt;
        entry.fmt.ptd = std::ptr::null_mut();
        if let Some(index) = existing {
            dynamic[index] = entry;
        } else {
            dynamic.push(entry);
        }
        drop(dynamic);
        if frelease.as_bool() {
            unsafe {
                ReleaseStgMedium(pmedium.cast_mut());
            }
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(Error::from(OLE_S_USEREG));
        }
        let mut formats: Vec<FORMATETC> = self.formats.iter().map(|e| e.fmt).collect();
        formats.extend(self.dynamic.borrow().iter().map(|entry| entry.fmt));
        Ok(AtlasEnumFormatEtc {
            formats,
            index: std::cell::Cell::new(0),
        }
        .into())
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Ref<'_, IAdviseSink>,
    ) -> Result<u32> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    #[test]
    fn dynamic_hglobal_is_cloned_replaced_enumerated_and_released() {
        unsafe {
            let object = AtlasDataObject::new(
                &[PathBuf::from(r"C:\fixture.txt")],
                windows::Win32::System::Ole::DROPEFFECT_COPY,
                Arc::new(AtomicU32::new(0)),
            )
            .unwrap();
            let id = windows::Win32::System::DataExchange::RegisterClipboardFormatW(
                windows::core::w!("AtlasDynamicOwnershipTest"),
            ) as u16;
            let format = FORMATETC {
                cfFormat: id,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            for (value, release) in [(42u32, false), (97u32, true)] {
                let original = build_u32_hglobal(value);
                let mut medium: STGMEDIUM = std::mem::zeroed();
                medium.tymed = TYMED_HGLOBAL.0 as u32;
                medium.u.hGlobal = HGLOBAL(original as *mut _);
                object.SetData(&format, &medium, release).unwrap();
                if !release {
                    ReleaseStgMedium(&mut medium);
                }
                object.QueryGetData(&format).ok().unwrap();
                let mut copied = object.GetData(&format).unwrap();
                let pointer = GlobalLock(copied.u.hGlobal) as *const u32;
                assert!(!pointer.is_null());
                assert_eq!(pointer.read(), value);
                let _ = GlobalUnlock(copied.u.hGlobal);
                ReleaseStgMedium(&mut copied);
            }
            let formats = object.EnumFormatEtc(DATADIR_GET.0 as u32).unwrap();
            let mut found = false;
            loop {
                let mut item: FORMATETC = std::mem::zeroed();
                let mut fetched = 0;
                if formats
                    .Next(std::slice::from_mut(&mut item), Some(&mut fetched))
                    .is_err()
                    || fetched == 0
                {
                    break;
                }
                found |= item.cfFormat == id;
            }
            assert!(found);
        }
    }
}

// ─── AtlasEnumFormatEtc ─────────────────────────────────────────────────────

/// Minimal `IEnumFORMATETC` over a snapshot of the data object's format list.
#[implement(IEnumFORMATETC)]
struct AtlasEnumFormatEtc {
    formats: Vec<FORMATETC>,
    index: std::cell::Cell<usize>,
}

impl IEnumFORMATETC_Impl for AtlasEnumFormatEtc_Impl {
    fn Next(&self, celt: u32, rgelt: *mut FORMATETC, pceltfetched: *mut u32) -> HRESULT {
        if rgelt.is_null() {
            return E_POINTER;
        }
        let current = self.index.get();
        let remaining = self.formats.len().saturating_sub(current);
        let to_copy = (celt as usize).min(remaining);
        for i in 0..to_copy {
            unsafe { rgelt.add(i).write(self.formats[current + i]) };
        }
        self.index.set(current + to_copy);
        if !pceltfetched.is_null() {
            unsafe { pceltfetched.write(to_copy as u32) };
        }
        if to_copy == celt as usize {
            HRESULT(0) // S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        let new_idx = (self.index.get() + celt as usize).min(self.formats.len());
        self.index.set(new_idx);
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.index.set(0);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumFORMATETC> {
        Ok(AtlasEnumFormatEtc {
            formats: self.formats.clone(),
            index: std::cell::Cell::new(self.index.get()),
        }
        .into())
    }
}

// ─── HRESULT constants not in windows-rs (or inconvenient to import) ────────

// DV_E_FORMATETC — the requested format is not available.
const DV_E_FORMATETC: HRESULT = HRESULT(0x8004_0064_u32 as i32);
// DATA_E_FORMATETC — same value, alternate name.
const DATA_E_FORMATETC: HRESULT = DV_E_FORMATETC;
// DATA_S_SAMEFORMATETC — GetCanonicalFormatEtc: caller should use original.
const DATA_S_SAMEFORMATETC: HRESULT = HRESULT(0x0004_0130_u32 as i32);
// OLE_S_USEREG — use the registry for this operation.
const OLE_S_USEREG: HRESULT = HRESULT(0x0004_0000_u32 as i32);
// OLE_E_ADVISENOTSUPPORTED
const OLE_E_ADVISENOTSUPPORTED: HRESULT = HRESULT(0x8004_0003_u32 as i32);
