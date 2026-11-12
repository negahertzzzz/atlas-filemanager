use super::{Command, Settings};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn image(command: &Command, settings: &Settings, output: &Path) -> Result<()> {
    write_image(decode_oriented(&command.source)?, command, settings, output)
}

/// Decode with a 512 MiB allocation cap and the EXIF orientation applied.
pub(super) fn decode_oriented(source: &Path) -> Result<image::DynamicImage> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::open(source)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

pub(super) fn write_image(
    mut img: image::DynamicImage,
    command: &Command,
    settings: &Settings,
    output: &Path,
) -> Result<()> {
    use image::ImageEncoder;
    let edge = if command.target.extension == "ico" {
        Some(settings.max_edge.unwrap_or(256).min(256))
    } else {
        settings.max_edge
    };
    if let Some(edge) = edge {
        img = img.thumbnail(edge.max(1), edge.max(1));
    }
    let mut file = File::create(output)?;
    if ["jpg", "pdf"].contains(&command.target.extension) {
        // JPEG has no alpha. Composite over white instead of turning transparent
        // pixels black; always feed the encoder RGB, never RGBA.
        let mut rgb = image::RgbImage::new(img.width(), img.height());
        for (src, dst) in img.to_rgba8().pixels().zip(rgb.pixels_mut()) {
            let a = u32::from(src[3]);
            for c in 0..3 {
                dst[c] = ((u32::from(src[c]) * a + 255 * (255 - a) + 127) / 255) as u8;
            }
        }
        if command.target.extension == "pdf" {
            write_image_pdf(&rgb, &mut file)?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut file,
                settings.quality.clamp(1, 100),
            )
            .encode_image(&rgb)?;
        }
    } else {
        // Keep this dispatch explicit. DynamicImage::write_to links every
        // encoder enabled anywhere in GPUI's unified image dependency graph,
        // including AV1/AVIF and EXR encoders this feature never uses.
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        let color = image::ExtendedColorType::Rgba8;
        match command.target.extension {
            "png" => image::codecs::png::PngEncoder::new(&mut file)
                .write_image(&rgba, width, height, color)?,
            "webp" => image::codecs::webp::WebPEncoder::new_lossless(&mut file)
                .write_image(&rgba, width, height, color)?,
            "bmp" => image::codecs::bmp::BmpEncoder::new(&mut file)
                .write_image(&rgba, width, height, color)?,
            "tiff" => image::codecs::tiff::TiffEncoder::new(&mut file)
                .write_image(&rgba, width, height, color)?,
            "gif" => image::codecs::gif::GifEncoder::new(&mut file)
                .encode_frame(image::Frame::new(rgba))?,
            "ico" => image::codecs::ico::IcoEncoder::new(&mut file)
                .write_image(&rgba, width, height, color)?,
            _ => bail!("Unsupported native image output"),
        }
    }
    Ok(())
}

fn write_image_pdf(image: &image::RgbImage, output: &mut File) -> Result<()> {
    use std::io::Seek;
    let mut compressed =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    compressed.write_all(image.as_raw())?;
    let compressed = compressed.finish()?;
    let width = image.width();
    let height = image.height();
    let page_width = (f64::from(width) * 0.75).min(14400.0);
    let page_height = page_width * f64::from(height) / f64::from(width);
    let content = format!("q {page_width} 0 0 {page_height} 0 0 cm /Im0 Do Q\n");
    let mut offsets = Vec::new();
    output.write_all(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;
    for (index, body) in [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_width} {page_height}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>"),
    ].iter().enumerate() {
        offsets.push(output.stream_position()?); writeln!(output, "{} 0 obj\n{body}\nendobj", index + 1)?;
    }
    offsets.push(output.stream_position()?);
    writeln!(output, "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream", compressed.len())?;
    output.write_all(&compressed)?;
    output.write_all(b"\nendstream\nendobj\n")?;
    offsets.push(output.stream_position()?);
    writeln!(
        output,
        "5 0 obj\n<< /Length {} >>\nstream\n{content}endstream\nendobj",
        content.len()
    )?;
    let xref = output.stream_position()?;
    output.write_all(b"xref\n0 6\n0000000000 65535 f \n")?;
    for offset in offsets {
        writeln!(output, "{offset:010} 00000 n ")?;
    }
    writeln!(
        output,
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF"
    )?;
    Ok(())
}

pub(super) fn extracted_document(text: &str, extension: &str, output: &Path) -> Result<()> {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    match extension {
        "txt" | "md" => std::fs::write(output, text)?,
        "html" => {
            let mut file = File::create(output)?;
            file.write_all(b"<!doctype html>\n<html><head><meta charset=\"utf-8\"></head><body>")?;
            for (index, page) in text.split('\u{c}').enumerate() {
                let style = if index == 0 {
                    ""
                } else {
                    " style=\"break-before:page\""
                };
                write!(
                    file,
                    "<section{style}><pre>{}</pre></section>\n",
                    escape(page)
                )?;
            }
            file.write_all(b"</body></html>\n")?;
        }
        "docx" => {
            let mut zip = zip::ZipWriter::new(File::create(output)?);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in [
                ("[Content_Types].xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>"),
                ("_rels/.rels", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>"),
            ] { zip.start_file(name, options)?; zip.write_all(data.as_bytes())?; }
            zip.start_file("word/document.xml", options)?;
            zip.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>")?;
            for (page, content) in text.split('\u{c}').enumerate() {
                if page > 0 {
                    zip.write_all(b"<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>")?;
                }
                for line in content.lines() {
                    zip.write_all(b"<w:p><w:r>")?;
                    for (index, part) in line.split('\t').enumerate() {
                        if index > 0 {
                            zip.write_all(b"<w:tab/>")?;
                        }
                        let part: String = part.chars().filter(|c| *c >= ' ').collect();
                        write!(zip, "<w:t xml:space=\"preserve\">{}</w:t>", escape(&part))?;
                    }
                    zip.write_all(b"</w:r></w:p>")?;
                }
            }
            zip.write_all(b"<w:sectPr/></w:body></w:document>")?;
            zip.finish()?;
        }
        _ => bail!("Unsupported extracted-text format"),
    }
    Ok(())
}

pub(super) fn text(command: &Command, output: &Path) -> Result<()> {
    extracted_document(
        &document_text(&command.source)?,
        command.target.extension,
        output,
    )
}

pub(super) fn document_text(source: &Path) -> Result<String> {
    let content = if super::catalog::extension(source) == "txt" {
        read_text(source)?
    } else {
        let mut zip = zip::ZipArchive::new(File::open(source)?)?;
        let xml = zip.by_name("word/document.xml")?;
        if xml.size() > 64 * 1024 * 1024 {
            bail!("Document text is limited to 64 MiB");
        }
        let mut content = String::new();
        xml.take(64 * 1024 * 1024 + 1)
            .read_to_string(&mut content)?;
        if content.len() > 64 * 1024 * 1024 {
            bail!("Document text is too large");
        }
        let document = roxmltree::Document::parse(&content)?;
        let mut result = String::new();
        for paragraph in document
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "p")
        {
            for node in paragraph.descendants().filter(roxmltree::Node::is_element) {
                match node.tag_name().name() {
                    "t" => result.push_str(node.text().unwrap_or_default()),
                    "tab" => result.push('\t'),
                    "br" if node
                        .attributes()
                        .any(|a| a.name() == "type" && a.value() == "page") =>
                    {
                        result.push('\u{c}')
                    }
                    "lastRenderedPageBreak" => result.push('\u{c}'),
                    "br" | "cr" => result.push('\n'),
                    _ => {}
                }
            }
            result.push('\n');
        }
        result
    };
    Ok(content)
}

pub(super) fn read_text(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    if file.metadata()?.len() > 64 * 1024 * 1024 {
        bail!("Text conversion is limited to 64 MiB per file");
    }
    let mut content = String::new();
    file.take(64 * 1024 * 1024 + 1)
        .read_to_string(&mut content)
        .context("Expected UTF-8 text")?;
    if content.len() > 64 * 1024 * 1024 {
        bail!("Text conversion is limited to 64 MiB per file");
    }
    Ok(content.trim_start_matches('\u{feff}').to_owned())
}

pub(super) fn data(command: &Command, output: &Path) -> Result<()> {
    let text = read_text(&command.source)?;
    let value = match super::catalog::extension(&command.source).as_str() {
        "json" => serde_json::from_str(&text)?,
        "jsonl" => serde_json::Value::Array(
            text.lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        "toml" => serde_json::to_value(toml::from_str::<toml::Value>(&text)?)?,
        "csv" | "tsv" => {
            let delimiter = if super::catalog::extension(&command.source) == "tsv" {
                '\t'
            } else {
                ','
            };
            let rows = parse_delimited(&text, delimiter)?;
            let mut iter = rows.into_iter();
            let headers = iter.next().context("The table is empty")?;
            if headers.iter().collect::<BTreeSet<_>>().len() != headers.len() {
                bail!("Table headers must be unique");
            }
            let mut records = Vec::new();
            for row in iter {
                if row.len() != headers.len() {
                    bail!(
                        "Table row has {} columns; expected {}",
                        row.len(),
                        headers.len()
                    );
                }
                records.push(serde_json::Value::Object(
                    headers
                        .iter()
                        .cloned()
                        .zip(row.into_iter().map(serde_json::Value::String))
                        .collect(),
                ));
            }
            serde_json::Value::Array(records)
        }
        _ => bail!("Unsupported data source"),
    };
    write_value(&value, command.target.extension, output)
}

fn as_tabular_rows<'a>(
    value: &'a serde_json::Value,
) -> Option<std::borrow::Cow<'a, [serde_json::Value]>> {
    if let Some(arr) = value.as_array() {
        return Some(std::borrow::Cow::Borrowed(arr));
    }
    if let Some(obj) = value.as_object() {
        if let Some(arr) = obj.get("rows").and_then(|v| v.as_array()) {
            return Some(std::borrow::Cow::Borrowed(arr));
        }
        for val in obj.values() {
            if let Some(arr) = val.as_array() {
                if !arr.is_empty() && arr.iter().all(|x| x.is_object()) {
                    return Some(std::borrow::Cow::Borrowed(arr));
                }
            }
        }
        if !obj.is_empty() && obj.values().all(|v| !v.is_object() && !v.is_array()) {
            return Some(std::borrow::Cow::Owned(vec![value.clone()]));
        }
    }
    None
}

fn write_value(value: &serde_json::Value, extension: &str, output: &Path) -> Result<()> {
    let mut file = File::create(output)?;
    match extension {
        "json" => serde_json::to_writer_pretty(&mut file, value)?,
        "jsonl" => {
            let rows = as_tabular_rows(value).context("JSONL requires a JSON array")?;
            for row in rows.as_ref() {
                serde_json::to_writer(&mut file, row)?;
                writeln!(file)?;
            }
        }
        "toml" => {
            let toml_value = if value.is_array() {
                serde_json::json!({ "rows": value })
            } else {
                value.clone()
            };
            file.write_all(
                toml::to_string_pretty(&toml_value)
                    .context("TOML needs a table and does not support null values")?
                    .as_bytes(),
            )?;
        }
        "csv" | "tsv" => {
            let rows = as_tabular_rows(value).context("CSV/TSV requires an array of objects")?;
            let mut headers = BTreeSet::new();
            for row in rows.as_ref() {
                headers.extend(
                    row.as_object()
                        .context("Every row must be an object")?
                        .keys()
                        .cloned(),
                );
            }
            let headers: Vec<_> = headers.into_iter().collect();
            let delimiter = if extension == "tsv" { '\t' } else { ',' };
            write_row(&mut file, &headers, delimiter)?;
            for row in rows.as_ref() {
                let cells = headers
                    .iter()
                    .map(|h| match &row[h] {
                        serde_json::Value::Null => String::new(),
                        serde_json::Value::String(s) => s.clone(),
                        v => v.to_string(),
                    })
                    .collect::<Vec<_>>();
                write_row(&mut file, &cells, delimiter)?;
            }
        }
        _ => bail!("Unsupported data target"),
    }
    Ok(())
}

/// RFC 4180 quoting, embedded newlines, CRLF, empty trailing fields and UTF-8.
pub(super) fn parse_delimited(text: &str, delimiter: char) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut closed = false;
    let mut at_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && at_start {
            quoted = true;
            at_start = false;
        } else if c == delimiter {
            row.push(std::mem::take(&mut field));
            at_start = true;
            closed = false;
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
            at_start = true;
            closed = false;
        } else {
            if closed || c == '"' {
                bail!("Malformed quoted field in table");
            }
            field.push(c);
            at_start = false;
        }
    }
    if quoted {
        bail!("Unclosed quoted field in table");
    }
    if !at_start || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

pub(super) fn write_row(file: &mut File, row: &[String], delimiter: char) -> Result<()> {
    for (index, field) in row.iter().enumerate() {
        if index > 0 {
            write!(file, "{delimiter}")?;
        }
        if field.contains([delimiter, '"', '\n', '\r']) {
            write!(file, "\"{}\"", field.replace('"', "\"\""))?;
        } else {
            file.write_all(field.as_bytes())?;
        }
    }
    file.write_all(b"\r\n")?;
    Ok(())
}

pub(super) fn sheet(command: &Command, output: &Path) -> Result<()> {
    use calamine::Reader;
    let mut workbook = calamine::open_workbook_auto(&command.source)?;
    if command.target.extension == "json" {
        let mut sheets = BTreeMap::new();
        for name in workbook.sheet_names() {
            let range = workbook.worksheet_range(&name)?;
            sheets.insert(
                name,
                range
                    .rows()
                    .map(|r| r.iter().map(ToString::to_string).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
            );
        }
        serde_json::to_writer_pretty(File::create(output)?, &sheets)?;
    } else {
        let range = workbook
            .worksheet_range_at(0)
            .context("Workbook has no sheets")??;
        let mut file = File::create(output)?;
        for row in range.rows() {
            write_row(
                &mut file,
                &row.iter().map(ToString::to_string).collect::<Vec<_>>(),
                if command.target.extension == "tsv" {
                    '\t'
                } else {
                    ','
                },
            )?;
        }
    }
    Ok(())
}

pub(super) fn markdown(command: &Command, output: &Path) -> Result<()> {
    use pulldown_cmark::{Event, Options, Parser};
    let text = read_text(&command.source)?;
    let parser = Parser::new_ext(&text, Options::all());
    let mut result = String::new();
    if command.target.extension == "html" {
        result.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\"></head><body>\n");
        // Raw HTML remains escaped: an exported local document should not gain
        // executable content merely because the Markdown contains HTML tags.
        pulldown_cmark::html::push_html(
            &mut result,
            parser.map(|event| match event {
                Event::Html(t) | Event::InlineHtml(t) => Event::Text(t),
                other => other,
            }),
        );
        result.push_str("</body></html>\n");
    } else {
        result = markdown_text(&text);
    }
    std::fs::write(output, result)?;
    Ok(())
}

pub(super) fn markdown_text(text: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser};
    let mut result = String::new();
    for event in Parser::new_ext(text, Options::all()) {
        match event {
            Event::Text(t) | Event::Code(t) => result.push_str(&t),
            Event::SoftBreak | Event::HardBreak | Event::Rule => result.push('\n'),
            Event::End(
                pulldown_cmark::TagEnd::Paragraph
                | pulldown_cmark::TagEnd::Heading(_)
                | pulldown_cmark::TagEnd::CodeBlock
                | pulldown_cmark::TagEnd::Item
                | pulldown_cmark::TagEnd::TableRow,
            ) => {
                if !result.ends_with('\n') {
                    result.push('\n');
                }
            }
            _ => {}
        }
    }
    result
}

enum ArchiveWriter {
    Zip(zip::ZipWriter<File>),
    Tar(tar::Builder<File>),
    Gzip(tar::Builder<flate2::write::GzEncoder<File>>),
}
impl ArchiveWriter {
    fn entry(&mut self, path: &Path, size: u64, directory: bool, reader: impl Read) -> Result<()> {
        if !safe_member(path) {
            bail!("Unsafe archive member: {}", path.display());
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(if directory { 0 } else { size });
        header.set_mode(if directory { 0o755 } else { 0o644 });
        header.set_entry_type(if directory {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_cksum();
        match self {
            Self::Zip(zip) => {
                let name = path.to_string_lossy().replace('\\', "/");
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                if directory {
                    zip.add_directory(name, options)?;
                } else {
                    zip.start_file(name, options)?;
                    std::io::copy(&mut { reader }, zip)?;
                }
            }
            Self::Tar(tar) => tar.append_data(&mut header, path, reader)?,
            Self::Gzip(tar) => tar.append_data(&mut header, path, reader)?,
        }
        Ok(())
    }
    fn finish(self) -> Result<()> {
        match self {
            Self::Zip(zip) => {
                zip.finish()?;
            }
            Self::Tar(tar) => {
                tar.into_inner()?;
            }
            Self::Gzip(tar) => {
                tar.into_inner()?.finish()?;
            }
        }
        Ok(())
    }
}

pub(super) fn safe_member(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|p| matches!(p, Component::Normal(_)))
        && !path.to_string_lossy().contains(['\\', ':'])
}

struct CancelReader<'a, R> {
    reader: R,
    cancel: &'a AtomicBool,
}
impl<R: Read> Read for CancelReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Cancelled"));
        }
        self.reader.read(buf)
    }
}

/// Repack entries as a stream; never extract an archive into the filesystem.
pub(super) fn archive(command: &Command, output: &Path, cancel: &AtomicBool) -> Result<()> {
    let file = File::create(output)?;
    let mut writer = match command.target.extension {
        "zip" => ArchiveWriter::Zip(zip::ZipWriter::new(file)),
        "tar" => ArchiveWriter::Tar(tar::Builder::new(file)),
        "tar.gz" => ArchiveWriter::Gzip(tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::default(),
        ))),
        _ => bail!("Unsupported archive output"),
    };
    let input = File::open(&command.source)?;
    if super::catalog::extension(&command.source) == "zip" {
        let mut zip = zip::ZipArchive::new(input)?;
        if zip.len() > 100_000 {
            bail!("Archive contains more than 100,000 entries");
        }
        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            // `enclosed_name` is the escape check; the stored name is what is
            // repacked (`safe_member` then also refuses absolute names, which
            // `enclosed_name` would silently make relative).
            let stored = entry.name()?.into_owned();
            entry.enclosed_name().context("Unsafe archive path")?;
            let path = std::path::PathBuf::from(stored);
            if entry.is_symlink() {
                bail!("Symbolic links are not supported during repacking");
            }
            writer.entry(
                &path,
                entry.size(),
                entry.is_dir(),
                CancelReader {
                    reader: entry,
                    cancel,
                },
            )?;
        }
    } else {
        let reader: Box<dyn Read> = match super::catalog::extension(&command.source).as_str() {
            "tar.gz" => Box::new(flate2::read::GzDecoder::new(input)),
            "tar.bz2" => Box::new(bzip2::read::BzDecoder::new(input)),
            "tar.xz" => Box::new(xz2::read::XzDecoder::new(input)),
            _ => Box::new(input),
        };
        let mut tar = tar::Archive::new(reader);
        for (index, entry) in tar.entries()?.enumerate() {
            if index >= 100_000 {
                bail!("Archive contains more than 100,000 entries");
            }
            let entry = entry?;
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() {
                bail!("Links and special archive entries are not supported");
            }
            let path = entry.path()?.into_owned();
            writer.entry(
                &path,
                entry.size(),
                kind.is_dir(),
                CancelReader {
                    reader: entry,
                    cancel,
                },
            )?;
        }
    }
    writer.finish()
}
