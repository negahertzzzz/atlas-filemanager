//! Shell-owned drag labels. The IDataObject receives its own HGLOBAL copy.
#![allow(unsafe_code)]
use crate::dnd::DropEffect;
use std::path::Path;
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Com::{
    IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::ReleaseStgMedium;
use windows::Win32::UI::Shell::{
    DROPDESCRIPTION, DROPIMAGE_COPY, DROPIMAGE_INVALID, DROPIMAGE_MOVE,
};

fn write_text(output: &mut [u16], text: &str) {
    let mut used = 0;
    for ch in text.chars().filter(|ch| *ch != '\0') {
        let mut encoded = [0; 2];
        let units = ch.encode_utf16(&mut encoded);
        if used + units.len() >= output.len() {
            break;
        }
        output[used..used + units.len()].copy_from_slice(units);
        used += units.len();
    }
}

pub(super) fn publish(
    object: &IDataObject,
    effect: Option<DropEffect>,
    destination: Option<&Path>,
) {
    let mut description = DROPDESCRIPTION {
        r#type: match effect {
            Some(DropEffect::Copy) => DROPIMAGE_COPY,
            Some(DropEffect::Move) => DROPIMAGE_MOVE,
            None => DROPIMAGE_INVALID,
        },
        ..Default::default()
    };
    if let (Some(effect), Some(destination)) = (effect, destination) {
        let message = if effect == DropEffect::Move {
            rust_i18n::t!("drop_description_move")
        } else {
            rust_i18n::t!("drop_description_copy")
        };
        let mut message_units = [0; 260];
        let mut destination_units = [0; 260];
        write_text(&mut message_units, &message);
        write_text(
            &mut destination_units,
            &destination
                .file_name()
                .unwrap_or_else(|| destination.as_os_str())
                .to_string_lossy(),
        );
        description.szMessage = message_units;
        description.szInsert = destination_units;
    }
    unsafe {
        let bytes = std::slice::from_raw_parts(
            (&raw const description).cast::<u8>(),
            std::mem::size_of::<DROPDESCRIPTION>(),
        );
        let allocation = super::data_object::build_bytes_hglobal(bytes);
        if allocation == 0 {
            return;
        }
        let format = FORMATETC {
            cfFormat: super::clipboard_formats::cf_drop_description(),
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let mut medium: STGMEDIUM = std::mem::zeroed();
        medium.tymed = TYMED_HGLOBAL.0 as u32;
        medium.u.hGlobal = HGLOBAL(allocation as *mut _);
        // Keep ownership locally. SetData clones the block when it accepts it.
        let _ = object.SetData(&format, &medium, false);
        ReleaseStgMedium(&mut medium);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_drop_description_roundtrips_copy_move_and_clear_through_data_object() {
        use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
        let object = super::super::data_object::AtlasDataObject::new(
            &[std::path::PathBuf::from(r"C:\fixture.txt")],
            windows::Win32::System::Ole::DROPEFFECT_COPY,
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        )
        .unwrap();
        let format = FORMATETC {
            cfFormat: super::super::clipboard_formats::cf_drop_description(),
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        for (effect, kind) in [
            (Some(DropEffect::Copy), DROPIMAGE_COPY),
            (Some(DropEffect::Move), DROPIMAGE_MOVE),
            (None, DROPIMAGE_INVALID),
        ] {
            publish(&object, effect, Some(Path::new(r"C:\Destino 日本語")));
            unsafe {
                object.QueryGetData(&format).ok().unwrap();
                let mut medium = object.GetData(&format).unwrap();
                let raw = GlobalLock(medium.u.hGlobal).cast::<DROPDESCRIPTION>();
                assert!(!raw.is_null());
                let description = raw.read();
                let _ = GlobalUnlock(medium.u.hGlobal);
                ReleaseStgMedium(&mut medium);
                let image_type = description.r#type;
                let message = description.szMessage;
                let insert = description.szInsert;
                assert_eq!(image_type, kind);
                let text = |s: &[u16]| {
                    String::from_utf16(&s[..s.iter().position(|c| *c == 0).unwrap()]).unwrap()
                };
                if effect.is_some() {
                    assert!(text(&message).contains("%1"));
                    assert_eq!(text(&insert), "Destino 日本語");
                } else {
                    assert!(text(&message).is_empty());
                }
            }
        }
    }
    #[test]
    fn truncating_a_shell_label_keeps_utf16_valid_and_null_terminated() {
        let mut text = [0; 5];
        write_text(&mut text, "ab🐙cd");
        assert_eq!(String::from_utf16(&text[..4]).unwrap(), "ab🐙");
        assert_eq!(text[4], 0);
    }
}
