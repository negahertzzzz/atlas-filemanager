//! On-demand PDF rasterization using the Windows renderer. No PDF engine payload.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::HSTRING;
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::StorageFile;
use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};

pub fn ocr_available() -> bool {
    OcrEngine::TryCreateFromUserProfileLanguages().is_ok()
}

/// Move a completed page folder without replacing any file, directory or link.
pub fn publish_pages(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // std::fs::rename uses MOVEFILE_REPLACE_EXISTING, which is inappropriate here.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(source.as_ptr(), target.as_ptr(), 0)
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Windows OCR is an optional OS capability; it never downloads a language pack.
pub fn recognize_pages(
    source: &Path,
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let recognize = || -> Result<String, Box<dyn std::error::Error>> {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| {
            "Windows OCR requires a recognition language installed in Windows Settings"
        })?;
        let maximum = OcrEngine::MaxImageDimension()?;
        let pages = render(source, directory, Some(maximum), cancel)?;
        let mut text = String::new();
        for (index, page) in pages.iter().enumerate() {
            let path = &page.path;
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled".into());
            }
            let file =
                StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.get()?;
            let decoder = BitmapDecoder::CreateAsync(&file.OpenReadAsync()?.get()?)?.get()?;
            let bitmap = decoder
                .GetSoftwareBitmapConvertedAsync(
                    BitmapPixelFormat::Bgra8,
                    BitmapAlphaMode::Premultiplied,
                )?
                .get()?;
            let result = engine.RecognizeAsync(&bitmap)?.get()?;
            bitmap.Close()?;
            if index > 0 {
                text.push('\u{c}');
            }
            for line in result.Lines()? {
                text.push_str(&line.Text()?.to_string_lossy());
                text.push('\n');
            }
            std::fs::remove_file(path)?;
            if text.len() > 64 * 1024 * 1024 {
                return Err("Recognized PDF text exceeds 64 MiB".into());
            }
        }
        if text
            .trim_matches(|c: char| c.is_whitespace() || c == '\u{c}')
            .is_empty()
        {
            return Err("Windows OCR did not recognize text in this PDF".into());
        }
        Ok(text)
    };
    recognize().map_err(|error| format!("Could not recognize PDF text: {error}"))
}

/// Render page 1 to in-memory PNG bytes with the built-in Windows PDF renderer,
/// scaled so the longest edge is `max_edge`. Used by the default preview path.
pub fn render_first_page_png(source: &Path, max_edge: u32) -> Result<Vec<u8>, String> {
    let render = || -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let path = std::path::absolute(source)?;
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.get()?;
        let document = PdfDocument::LoadFromFileAsync(&file)?.get()?;
        if document.PageCount()? == 0 {
            return Err("PDF has no pages".into());
        }
        let page = document.GetPage(0)?;
        let result = (|| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            let size = page.Size()?;
            let longest = f64::from(size.Width.max(size.Height));
            if !longest.is_finite() || longest <= 0.0 {
                return Err("Invalid page size".into());
            }
            let scale = f64::from(max_edge.max(1)) / longest;
            let width = (f64::from(size.Width) * scale).round().clamp(1.0, 8192.0);
            let height = (f64::from(size.Height) * scale).round().clamp(1.0, 8192.0);
            let options = PdfPageRenderOptions::new()?;
            options.SetDestinationWidth(width as u32)?;
            options.SetDestinationHeight(height as u32)?;
            let stream = InMemoryRandomAccessStream::new()?;
            page.RenderWithOptionsToStreamAsync(&stream, &options)?
                .get()?;
            let length = stream.Size()?;
            if length == 0 || length > 256 * 1024 * 1024 {
                return Err("Rendered page is too large or empty".into());
            }
            let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
            let loaded = reader.LoadAsync(length as u32)?.get()?;
            let mut bytes = vec![0; loaded as usize];
            reader.ReadBytes(&mut bytes)?;
            Ok(bytes)
        })();
        let _ = page.Close();
        result
    };
    render().map_err(|error| format!("Could not render PDF: {error}"))
}

/// Render every page to a PNG in an isolated directory owned by the caller.
pub struct RenderedPage {
    pub path: PathBuf,
    pub dpi: u16,
}

pub fn render_pages(
    source: &Path,
    directory: &Path,
    max_edge: Option<u32>,
    cancel: &AtomicBool,
) -> Result<Vec<RenderedPage>, String> {
    render(source, directory, max_edge, cancel)
        .map_err(|error| format!("Could not render PDF: {error}"))
}

fn render(
    source: &Path,
    directory: &Path,
    max_edge: Option<u32>,
    cancel: &AtomicBool,
) -> Result<Vec<RenderedPage>, Box<dyn std::error::Error>> {
    let check = || -> Result<(), Box<dyn std::error::Error>> {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        Ok(())
    };
    check()?;
    let path = std::path::absolute(source)?;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.get()?;
    let document = PdfDocument::LoadFromFileAsync(&file)?.get()?;
    let count = document.PageCount()?;
    if count == 0 || count > 10_000 {
        return Err("PDF must contain 1–10000 pages".into());
    }
    let mut pages = Vec::new();
    for index in 0..count {
        check()?;
        let page = document.GetPage(index)?;
        // PdfPage is IClosable; close even on rendering/read/write errors.
        let result = (|| -> Result<RenderedPage, Box<dyn std::error::Error>> {
            let size = page.Size()?;
            let mut width = (f64::from(size.Width) * 300.0 / 96.0).ceil();
            let mut height = (f64::from(size.Height) * 300.0 / 96.0).ceil();
            if let Some(edge) = max_edge {
                if edge == 0 {
                    return Err("Page size must be greater than zero".into());
                }
                let scale = (f64::from(edge) / width.max(height)).min(1.0);
                width = (width * scale).round().max(1.0);
                height = (height * scale).round().max(1.0);
            }
            if !width.is_finite()
                || !height.is_finite()
                || width < 1.0
                || height < 1.0
                || width > 32768.0
                || height > 32768.0
                || width * height > 64_000_000.0
            {
                return Err(
                    "PDF page exceeds the rasterization limit; choose a smaller size".into(),
                );
            }
            let options = PdfPageRenderOptions::new()?;
            options.SetDestinationWidth(width as u32)?;
            options.SetDestinationHeight(height as u32)?;
            let stream = InMemoryRandomAccessStream::new()?;
            page.RenderWithOptionsToStreamAsync(&stream, &options)?
                .get()?;
            check()?;
            let length = stream.Size()?;
            if length == 0 || length > 256 * 1024 * 1024 {
                return Err("Rendered page is too large or empty".into());
            }
            let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
            let loaded = reader.LoadAsync(length as u32)?.get()?;
            if u64::from(loaded) != length {
                return Err("Incomplete rendered page".into());
            }
            let mut bytes = vec![0; length as usize];
            reader.ReadBytes(&mut bytes)?;
            let dpi = (width * 96.0 / f64::from(size.Width))
                .round()
                .clamp(1.0, 65535.0) as u16;
            png_density(&mut bytes, dpi)?;
            let path = directory.join(format!("page-{:04}.png", index + 1));
            std::fs::write(&path, bytes)?;
            Ok(RenderedPage { path, dpi })
        })();
        let closed = page.Close();
        pages.push(result?);
        closed?;
    }
    check()?;
    Ok(pages)
}

// Set PNG physical resolution as well as raster dimensions, without re-encoding.
fn png_density(bytes: &mut Vec<u8>, dpi: u16) -> Result<(), Box<dyn std::error::Error>> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Windows returned a non-PNG page".into());
    }
    let ppm = (f64::from(dpi) / 0.0254).round() as u32;
    let mut chunk = Vec::from(9_u32.to_be_bytes());
    chunk.extend_from_slice(b"pHYs");
    chunk.extend_from_slice(&ppm.to_be_bytes());
    chunk.extend_from_slice(&ppm.to_be_bytes());
    chunk.push(1);
    let mut crc = !0_u32;
    for byte in &chunk[4..] {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    chunk.extend_from_slice(&(!crc).to_be_bytes());
    let mut offset = 8_usize;
    while offset.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into()?) as usize;
        let end = offset
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .ok_or("PNG chunk overflow")?;
        if end > bytes.len() {
            return Err("Truncated PNG page".into());
        }
        match &bytes[offset + 4..offset + 8] {
            b"pHYs" => {
                bytes.splice(offset..end, chunk);
                return Ok(());
            }
            b"IDAT" => {
                bytes.splice(offset..offset, chunk);
                return Ok(());
            }
            _ => {}
        }
        offset = end;
    }
    Err("Rendered PNG has no image data".into())
}
