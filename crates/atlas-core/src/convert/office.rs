//! OOXML package normalization, preserving content rather than flattening to text.
use super::{native::safe_member, Command};
use anyhow::{ensure, Context, Result};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const DOCX_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

fn xml_part(archive: &mut zip::ZipArchive<File>, name: &str) -> Result<String> {
    let part = archive.by_name(name)?;
    ensure!(part.size() <= 64 * 1024 * 1024, "Office XML exceeds 64 MiB");
    let mut text = String::new();
    part.take(64 * 1024 * 1024 + 1).read_to_string(&mut text)?;
    ensure!(text.len() <= 64 * 1024 * 1024, "Office XML exceeds 64 MiB");
    Ok(text)
}

fn rewrite(text: &str, mut edits: Vec<(std::ops::Range<usize>, String)>) -> String {
    edits.sort_by_key(|(range, _)| range.start);
    let mut result = text.to_owned();
    for (range, value) in edits.into_iter().rev() {
        result.replace_range(range, &value);
    }
    result
}

pub(super) fn normalize(command: &Command, output: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut source = zip::ZipArchive::new(File::open(&command.source)?)?;
    ensure!(
        source.len() <= 20_000,
        "Office package contains too many parts"
    );
    let mut names = BTreeSet::new();
    let mut expanded = 0_u64;
    for index in 0..source.len() {
        let part = source.by_index(index)?;
        let part_name = part.name()?;
        ensure!(
            safe_member(Path::new(part_name.as_ref())) && !part_name.contains('\\'),
            "Unsafe Office part path"
        );
        ensure!(
            names.insert(part_name.to_lowercase()),
            "Duplicate Office part"
        );
        expanded = expanded
            .checked_add(part.size())
            .context("Office package size overflow")?;
        ensure!(
            expanded <= 256 * 1024 * 1024,
            "Office package exceeds 256 MiB expanded"
        );
    }
    ensure!(
        names.contains("word/document.xml"),
        "Missing Word document part"
    );
    let main_xml = xml_part(&mut source, "word/document.xml")?;
    let main_document = roxmltree::Document::parse(&main_xml)?;
    ensure!(
        main_document.root_element().has_tag_name((
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
            "document"
        )),
        "Invalid Word document root"
    );
    let types = xml_part(&mut source, "[Content_Types].xml")?;
    let document = roxmltree::Document::parse(&types)?;
    let mut removed = BTreeSet::new();
    let mut edits = Vec::new();
    let mut main = false;
    let mut macro_defaults = BTreeSet::new();
    let mut overridden = BTreeSet::new();
    for node in document.descendants().filter(|node| {
        node.has_tag_name((
            "http://schemas.openxmlformats.org/package/2006/content-types",
            "Default",
        ))
    }) {
        let content = node
            .attribute("ContentType")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content.contains("vba") || content.contains("digital-signature") {
            macro_defaults.insert(
                node.attribute("Extension")
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            );
            edits.push((node.range(), String::new()));
        }
    }
    for node in document.descendants().filter(|node| {
        node.has_tag_name((
            "http://schemas.openxmlformats.org/package/2006/content-types",
            "Override",
        ))
    }) {
        let name = node
            .attribute("PartName")
            .unwrap_or_default()
            .trim_start_matches('/');
        let content = node.attribute("ContentType").unwrap_or_default();
        overridden.insert(name.to_ascii_lowercase());
        let content_lower = content.to_ascii_lowercase();
        if content_lower.contains("vba")
            || content_lower.contains("digital-signature")
            || name.to_ascii_lowercase().starts_with("_xmlsignatures/")
        {
            removed.insert(name.to_ascii_lowercase());
            edits.push((node.range(), String::new()));
        } else if name == "word/document.xml" {
            ensure!([DOCX_TYPE, "application/vnd.ms-word.document.macroEnabled.main+xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml"].contains(&content), "Unsupported Word main content type");
            main = true;
            edits.push((node.range(), format!("<Override xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\" PartName=\"/word/document.xml\" ContentType=\"{DOCX_TYPE}\"/>")));
        }
    }
    ensure!(main, "Missing Word main content type");
    for name in &names {
        if !overridden.contains(name)
            && name
                .rsplit_once('.')
                .is_some_and(|(_, extension)| macro_defaults.contains(extension))
        {
            removed.insert(name.clone());
        }
    }
    // These well-known macro parts can also be covered by a Default content type.
    removed.extend(
        [
            "word/vbaProject.bin",
            "word/vbaData.xml",
            "word/vbaProjectSignature.bin",
        ]
        .map(str::to_ascii_lowercase),
    );
    let types = rewrite(&types, edits);
    let mut target = zip::ZipWriter::new(File::create(output)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for index in 0..source.len() {
        ensure!(!cancel.load(Ordering::Relaxed), "Cancelled");
        let part = source.by_index(index)?;
        let name = part.name()?.into_owned();
        let lower = name.to_ascii_lowercase();
        let owner = name.rsplit_once("/_rels/").and_then(|(parent, file)| {
            file.strip_suffix(".rels")
                .map(|file| format!("{parent}/{file}"))
        });
        if removed.contains(&lower)
            || lower.starts_with("_xmlsignatures/")
            || owner.is_some_and(|owner| removed.contains(&owner.to_ascii_lowercase()))
        {
            continue;
        }
        if name == "[Content_Types].xml" {
            target.start_file(name, options)?;
            target.write_all(types.as_bytes())?;
        } else if name.ends_with(".rels") {
            drop(part);
            let text = xml_part(&mut source, &name)?;
            let document = roxmltree::Document::parse(&text)?;
            let edits = document
                .descendants()
                .filter(|node| {
                    let kind = node
                        .attribute("Type")
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    kind.contains("vbaproject")
                        || kind.contains("vbadata")
                        || kind.contains("digital-signature")
                })
                .map(|node| (node.range(), String::new()))
                .collect();
            target.start_file(name, options)?;
            target.write_all(rewrite(&text, edits).as_bytes())?;
        } else {
            target.raw_copy_file(part)?;
        }
    }
    target.finish()?;
    Ok(())
}
