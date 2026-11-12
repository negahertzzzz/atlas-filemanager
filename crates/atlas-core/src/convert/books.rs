//! Small text-based EPUB/FB2 readers and writers, implemented against XML/ZIP
//! specifications. No browser, Calibre, Pandoc or Office runtime is involved.
use super::{native, Command};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

const LIMIT: u64 = 64 * 1024 * 1024;

struct Book {
    title: String,
    text: String,
}

fn parse_xml(text: &str) -> Result<roxmltree::Document<'_>> {
    Ok(roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            nodes_limit: 1_000_000,
        },
    )?)
}

fn escape(text: &str) -> String {
    text.chars()
        .filter(|c| *c >= ' ' || matches!(c, '\n' | '\t'))
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_text(root: roxmltree::Node<'_, '_>) -> String {
    let mut result = String::new();
    let mut stack = vec![(root, false)];
    while let Some((node, closing)) = stack.pop() {
        if node.is_element()
            && ["head", "script", "style", "nav", "binary"].contains(&node.tag_name().name())
        {
            continue;
        }
        if !closing && node.is_text() {
            for c in node.text().unwrap_or_default().chars() {
                if c.is_whitespace() {
                    if !result.is_empty() && !result.ends_with([' ', '\n']) {
                        result.push(' ');
                    }
                } else {
                    result.push(c);
                }
            }
        } else if closing
            && node.is_element()
            && [
                "p",
                "div",
                "section",
                "title",
                "h1",
                "h2",
                "h3",
                "h4",
                "li",
                "tr",
                "br",
                "empty-line",
                "v",
                "subtitle",
            ]
            .contains(&node.tag_name().name())
        {
            if result.ends_with(' ') {
                result.pop();
            }
            if !result.ends_with('\n') {
                result.push('\n');
            }
        } else if !closing {
            stack.push((node, true));
            for child in node.children().rev() {
                stack.push((child, false));
            }
        }
    }
    result.trim().to_owned()
}

fn member(zip: &mut zip::ZipArchive<File>, name: &str, budget: &mut u64) -> Result<String> {
    let entry = zip
        .by_name(name)
        .with_context(|| format!("EPUB member not found: {name}"))?;
    ensure!(entry.size() <= *budget, "Expanded EPUB text exceeds 64 MiB");
    let mut text = String::new();
    entry
        .take(*budget + 1)
        .read_to_string(&mut text)
        .context("Expected UTF-8 EPUB XML")?;
    ensure!(
        text.len() as u64 <= *budget,
        "Expanded EPUB text exceeds 64 MiB"
    );
    *budget -= text.len() as u64;
    Ok(text)
}

/// Resolve only archive-local paths, including percent-encoded EPUB hrefs.
fn href(base: &str, reference: &str) -> Result<String> {
    let reference = reference.split('#').next().unwrap_or_default();
    let mut decoded = Vec::with_capacity(reference.len());
    let mut bytes = reference.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .context("Invalid EPUB percent encoding")?;
            let low = bytes
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .context("Invalid EPUB percent encoding")?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    let reference = std::str::from_utf8(&decoded)?;
    ensure!(
        !reference.contains([':', '\\', '\0', '?']),
        "EPUB reference must be a local archive path"
    );
    let mut parts: Vec<&str> = if reference.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map_or(Vec::new(), |(dir, _)| dir.split('/').collect())
    };
    for part in reference.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(
                    parts.pop().is_some(),
                    "EPUB reference escapes the archive root"
                );
            }
            part => parts.push(part),
        }
    }
    ensure!(!parts.is_empty(), "Empty EPUB member reference");
    Ok(parts.join("/"))
}

fn epub(path: &Path) -> Result<Book> {
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    ensure!(zip.len() <= 100_000, "EPUB has too many archive members");
    let mut names = std::collections::BTreeSet::new();
    for index in 0..zip.len() {
        let entry = zip.by_index(index)?;
        ensure!(
            names.insert(entry.name()?.into_owned()),
            "EPUB has duplicate archive members"
        );
    }
    let mut budget = LIMIT;
    let container = member(&mut zip, "META-INF/container.xml", &mut budget)?;
    let container = parse_xml(&container)?;
    let rootfile = container
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "rootfile")
        .and_then(|n| n.attribute("full-path"))
        .context("EPUB has no package rootfile")?;
    let package_path = href("", rootfile)?;
    let package = member(&mut zip, &package_path, &mut budget)?;
    let package = parse_xml(&package)?;
    ensure!(
        package.root_element().tag_name().name() == "package",
        "Invalid EPUB package document"
    );
    let title = package
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "title")
        .and_then(|n| n.text())
        .unwrap_or("Book")
        .to_owned();
    let manifest: std::collections::BTreeMap<_, _> = package
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "item")
        .filter_map(|n| {
            Some((
                n.attribute("id")?,
                (n.attribute("href")?, n.attribute("media-type")?),
            ))
        })
        .collect();
    let mut encrypted = std::collections::BTreeSet::new();
    if names.contains("META-INF/encryption.xml") {
        let xml = member(&mut zip, "META-INF/encryption.xml", &mut budget)?;
        let xml = parse_xml(&xml)?;
        for reference in xml
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "CipherReference")
            .filter_map(|n| n.attribute("URI"))
        {
            encrypted.insert(href("", reference)?);
        }
    }
    let mut text = String::new();
    let mut chapters = 0;
    for spine in package
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "itemref")
    {
        let (reference, kind) = manifest
            .get(
                spine
                    .attribute("idref")
                    .context("EPUB spine item has no idref")?,
            )
            .context("EPUB spine references a missing manifest item")?;
        ensure!(
            *kind == "application/xhtml+xml",
            "Native EPUB export supports XHTML reading-order documents"
        );
        let path = href(&package_path, reference)?;
        ensure!(
            !encrypted.contains(&path),
            "Encrypted EPUB content cannot be converted natively"
        );
        let chapter = member(&mut zip, &path, &mut budget)?;
        let chapter = parse_xml(&chapter)?;
        let body = chapter
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "body")
            .context("EPUB chapter has no XHTML body")?;
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&xml_text(body));
        chapters += 1;
        ensure!(chapters <= 100_000, "EPUB has too many reading-order items");
        ensure!(
            text.len() as u64 <= LIMIT,
            "Expanded EPUB text exceeds 64 MiB"
        );
    }
    ensure!(
        chapters > 0 && !text.trim().is_empty(),
        "EPUB has no readable text in its spine"
    );
    Ok(Book { title, text })
}

fn fb2(path: &Path) -> Result<Book> {
    let text = native::read_text(path)?;
    let xml = parse_xml(&text)?;
    ensure!(
        xml.root_element().tag_name().name() == "FictionBook",
        "Expected a FictionBook document"
    );
    let title = xml
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "book-title")
        .map_or_else(|| "Book".into(), xml_text);
    let text = xml
        .root_element()
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "body")
        .map(xml_text)
        .collect::<Vec<_>>()
        .join("\n\n");
    ensure!(
        !text.trim().is_empty(),
        "FictionBook has no readable body text"
    );
    Ok(Book { title, text })
}

fn modified_time() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut days = seconds / 86400;
    let mut year = 1970;
    let leap = |year| year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    while days >= if leap(year) { 366 } else { 365 } {
        days -= if leap(year) { 366 } else { 365 };
        year += 1;
    }
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while month < 11 && days >= months[month] {
        days -= months[month];
        month += 1;
    }
    format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        month + 1,
        days + 1,
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn write_epub(book: &Book, output: &Path) -> Result<()> {
    let mut zip = zip::ZipWriter::new(File::create(output)?);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/epub+zip")?;
    let title = escape(&book.title);
    let identifier = format!("urn:sha256:{:x}", Sha256::digest(book.text.as_bytes()));
    let container = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"EPUB/package.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>".to_owned();
    let package = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"id\"><metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier id=\"id\">{identifier}</dc:identifier><dc:title>{title}</dc:title><dc:language>und</dc:language><meta property=\"dcterms:modified\">{}</meta></metadata><manifest><item id=\"document\" href=\"document.xhtml\" media-type=\"application/xhtml+xml\"/><item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/></manifest><spine><itemref idref=\"document\"/></spine></package>", modified_time());
    let nav = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>{title}</title></head><body><nav epub:type=\"toc\"><h1>{title}</h1><ol><li><a href=\"document.xhtml\">{title}</a></li></ol></nav></body></html>");
    let paragraphs = book
        .text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("<p>{}</p>\n", escape(line)))
        .collect::<String>();
    let document = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{title}</title></head><body>{paragraphs}</body></html>");
    let compressed = stored.compression_method(zip::CompressionMethod::Deflated);
    for (name, text) in [
        ("META-INF/container.xml", container),
        ("EPUB/package.opf", package),
        ("EPUB/nav.xhtml", nav),
        ("EPUB/document.xhtml", document),
    ] {
        zip.start_file(name, compressed)?;
        zip.write_all(text.as_bytes())?;
    }
    zip.finish()?;
    Ok(())
}

pub(super) fn convert(command: &Command, output: &Path) -> Result<()> {
    let book = match super::catalog::extension(&command.source).as_str() {
        "epub" => epub(&command.source)?,
        "fb2" => fb2(&command.source)?,
        "txt" | "docx" | "docm" | "dotx" | "md" => {
            let text = if super::catalog::extension(&command.source) == "md" {
                native::markdown_text(&native::read_text(&command.source)?)
            } else {
                native::document_text(&command.source)?
            };
            ensure!(!text.trim().is_empty(), "Document has no readable text");
            Book {
                title: command
                    .source
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                text,
            }
        }
        _ => bail!("Unsupported native book input"),
    };
    match command.target.extension {
        "epub" => write_epub(&book, output),
        "fb2" => {
            let title = escape(&book.title);
            let identifier = format!("{:x}", Sha256::digest(book.text.as_bytes()));
            let mut file = File::create(output)?;
            write!(file, "<?xml version=\"1.0\" encoding=\"UTF-8\"?><FictionBook xmlns=\"http://www.gribuser.ru/xml/fictionbook/2.0\"><description><title-info><genre>other</genre><author><nickname>Unknown</nickname></author><book-title>{title}</book-title><lang>und</lang></title-info><document-info><author><nickname>Atlas</nickname></author><date>{}</date><id>{identifier}</id><version>1.0</version></document-info></description><body><section>", modified_time())?;
            for line in book.text.lines() {
                if line.is_empty() {
                    file.write_all(b"<empty-line/>")?;
                } else {
                    write!(file, "<p>{}</p>", escape(line))?;
                }
            }
            file.write_all(b"</section></body></FictionBook>")?;
            Ok(())
        }
        extension => native::extracted_document(&book.text, extension, output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_hrefs_are_local_normalized_and_percent_decoded() {
        assert_eq!(
            href("OEBPS/package.opf", "../Text/cap%C3%ADtulo.xhtml#title").unwrap(),
            "Text/capítulo.xhtml"
        );
        for reference in [
            "../../outside",
            "https://example.test/remote",
            "%2e%2e/%2e%2e/escape",
            "bad%ZZ",
            "file:%2fetc",
        ] {
            assert!(href("OEBPS/package.opf", reference).is_err(), "{reference}");
        }
    }
}
