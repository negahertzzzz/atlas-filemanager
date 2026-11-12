//! Read-only, system-formatted properties. Call only on a background worker.
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyDescription, IPropertyStore, PSFormatPropertyValue, PSGetPropertyDescriptionByName,
    SHGetPropertyStoreFromParsingName, GPS_DEFAULT, PDFF_DEFAULT,
};

const PROPERTIES: &[&str] = &[
    "System.Music.Artist",
    "System.Media.Duration",
    "System.Audio.EncodingBitrate",
    "System.Photo.CameraModel",
    "System.Photo.DateTaken",
    "System.Image.Dimensions",
    "System.Video.FrameWidth",
    "System.Video.FrameHeight",
    "System.Title",
    "System.Author",
    "System.Document.PageCount",
    "System.Document.WordCount",
];

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

// Both property formatting and display-name APIs allocate with CoTaskMemAlloc.
unsafe fn take_string(value: PWSTR) -> String {
    if value.is_null() {
        return String::new();
    }
    let text = unsafe { value.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    text
}

/// Missing handlers and absent properties are ordinary empty results. Localized
/// labels, dates, durations, bitrates and units come from Windows, not Atlas.
pub fn extended_properties(path: &Path) -> Vec<(String, String)> {
    if crate::directory::is_cloud_placeholder(path) {
        return Vec::new();
    }
    let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        // Tokio's blocking pool is normally uninitialized. If a caller already
        // owns an STA, use it without balancing an initialization that failed.
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _apartment = initialized.is_ok().then_some(Apartment);
        if initialized.is_err() && initialized != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Vec::new();
        }
        let Ok(store): windows::core::Result<IPropertyStore> =
            SHGetPropertyStoreFromParsingName(PCWSTR(path.as_ptr()), None, GPS_DEFAULT)
        else {
            return Vec::new();
        };
        PROPERTIES
            .iter()
            .filter_map(|name| {
                let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
                let mut raw = std::ptr::null_mut();
                PSGetPropertyDescriptionByName(
                    PCWSTR(wide.as_ptr()),
                    &IPropertyDescription::IID,
                    &mut raw,
                )
                .ok()?;
                if raw.is_null() {
                    return None;
                }
                let description = IPropertyDescription::from_raw(raw);
                let value =
                    take_string(PSFormatPropertyValue(&store, &description, PDFF_DEFAULT).ok()?);
                if value.trim().is_empty() {
                    return None;
                }
                let label = description
                    .GetDisplayName()
                    .ok()
                    .map(|v| take_string(v))
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| (*name).to_string());
                Some((label, value))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_formats_real_wave_duration_and_bitrate() {
        let path =
            std::env::temp_dir().join(format!("atlas-properties-{}.wav", std::process::id()));
        // One second of mono, 8-bit PCM at 8 kHz.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(8036u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.resize(8044, 128);
        std::fs::write(&path, wav).unwrap();
        let properties = extended_properties(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(properties.len() >= 2, "{properties:?}");
        assert!(properties
            .iter()
            .all(|(key, value)| !key.is_empty() && !value.is_empty()));
        assert!(
            properties
                .iter()
                .any(|(_, value)| value.contains("00:00:01")),
            "{properties:?}"
        );
    }

    #[test]
    fn windows_property_handlers_read_real_mp3_and_mp4_fixtures() {
        for (extension, bytes) in [
            (
                "mp3",
                include_bytes!("../tests/fixtures/metadata.mp3").as_slice(),
            ),
            (
                "mp4",
                include_bytes!("../tests/fixtures/metadata.mp4").as_slice(),
            ),
        ] {
            let path = std::env::temp_dir().join(format!(
                "atlas-properties-{}.{extension}",
                std::process::id()
            ));
            std::fs::write(&path, bytes).unwrap();
            let properties = extended_properties(&path);
            std::fs::remove_file(&path).unwrap();
            let has = |needle: &str| properties.iter().any(|(_, value)| value.contains(needle));
            assert!(has("00:00:01"), "{extension}: {properties:?}");
            if extension == "mp3" {
                assert!(has("Atlas Metadata Fixture"), "{properties:?}");
                assert!(has("128"), "{properties:?}");
            } else {
                assert!(has("32") && has("24"), "{properties:?}");
            }
        }
    }

    #[test]
    fn missing_file_has_no_properties() {
        assert!(extended_properties(Path::new(
            r"C:\atlas-missing-properties-fixture\missing.mp3"
        ))
        .is_empty());
    }
}
