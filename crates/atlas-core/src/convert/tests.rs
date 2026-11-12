use super::*;
use std::io::{Read, Write};

#[test]
fn native_policy_never_offers_or_runs_heavy_engines_even_if_installed() {
    let mut tools = Tools::default();
    for tool in Tool::ALL {
        tools.0.insert(tool, PathBuf::from(tool.executable()));
    }
    for extension in [
        "wav", "pdf", "docx", "pptx", "heic", "mobi", "7z", "epub", "txt",
    ] {
        let source = PathBuf::from(format!("fixture.{extension}"));
        for option in native_options(&[source], &tools) {
            assert!(
                option
                    .engines
                    .iter()
                    .all(|engine| ["Atlas", "Windows", "Windows OCR", "Opus"].contains(engine)),
                "{extension}: {:?}",
                option.engines
            );
            assert!(option.missing.iter().all(|tool| *tool == Tool::Opus));
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("test.heic");
    std::fs::write(&source, b"fixture").unwrap();
    assert!(Plan::build_native(&[source], "png", Settings::default(), tools).is_err());
}

#[test]
fn word_package_conversion_preserves_styles_tables_images_headers_and_removes_macros() {
    let dir = tempfile::tempdir().unwrap();
    let namespace = "http://schemas.openxmlformats.org/package/2006/content-types";
    let document = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body><w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Styled title</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="6000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Table cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="609600"/><wp:docPr id="1" name="Test image"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="0" name="image.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="image"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="609600"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="header"/><w:pgSz w:w="11906" w:h="16838"/></w:sectPr></w:body></w:document>"#;
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(12, 8, image::Rgb([50, 100, 150]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    for extension in ["docm", "dotx"] {
        let main_type = if extension == "docm" {
            "application/vnd.ms-word.document.macroEnabled.main+xml"
        } else {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml"
        };
        let types = format!(
            r#"<Types xmlns="{namespace}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="{main_type}"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/vbaProject.bin" ContentType="application/vnd.ms-office.vbaProject"/></Types>"#
        );
        let relationships = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="image" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image.png"/><Relationship Id="styles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="header" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="macro" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/><Relationship Id="signature" Type="http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin" Target="../_xmlsignatures/origin.sigs"/></Relationships>"#;
        let source = dir.path().join(format!("original.{extension}"));
        let preserved: &[(&str, &[u8])] = &[
            ("word/document.xml", document),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="document" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
            (
                "word/styles.xml",
                br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:rPr><w:sz w:val="40"/></w:rPr></w:style></w:styles>"#,
            ),
            ("word/media/image.png", png.get_ref()),
            ("word/header1.xml", br#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Original header</w:t></w:r></w:p></w:hdr>"#),
        ];
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
        for (name, bytes) in preserved.iter().copied().chain([
            ("[Content_Types].xml", types.as_bytes()),
            ("word/_rels/document.xml.rels", relationships.as_slice()),
            ("word/vbaProject.bin", b"macro".as_slice()),
            (
                "word/_rels/vbaProject.bin.rels",
                b"macro signature".as_slice(),
            ),
            ("_xmlsignatures/origin.sigs", b"signature".as_slice()),
        ]) {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        let original = std::fs::read(&source).unwrap();
        let report = run(&source, "docx");
        assert!(report.errors().is_empty(), "{:?}", report.errors());
        let mut zip =
            zip::ZipArchive::new(std::fs::File::open(&report.outputs()[0]).unwrap()).unwrap();
        for (name, expected) in preserved {
            let mut bytes = Vec::new();
            zip.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, *expected);
        }
        let mut types = String::new();
        zip.by_name("[Content_Types].xml")
            .unwrap()
            .read_to_string(&mut types)
            .unwrap();
        assert!(types.contains("wordprocessingml.document.main+xml"));
        assert!(!types.contains("macroEnabled"));
        let mut rels = String::new();
        zip.by_name("word/_rels/document.xml.rels")
            .unwrap()
            .read_to_string(&mut rels)
            .unwrap();
        let rels = roxmltree::Document::parse(&rels).unwrap();
        let rels: Vec<_> = rels
            .descendants()
            .filter(|n| n.tag_name().name() == "Relationship")
            .collect();
        assert_eq!(rels.len(), 3);
        assert_eq!(rels[0].attribute("Id"), Some("image"));
        assert!(!zip
            .file_names()
            .filter_map(Result::ok)
            .any(|n| n.contains("vbaProject") || n.starts_with("_xmlsignatures/")));
        assert_eq!(std::fs::read(source).unwrap(), original);
        if let Some(directory) = std::env::var_os("ATLAS_PAGE_TEST_ARTIFACTS") {
            let directory = PathBuf::from(directory).join("office");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(format!("original.{extension}")), &original).unwrap();
            std::fs::copy(
                &report.outputs()[0],
                directory.join(format!("from-{extension}.docx")),
            )
            .unwrap();
        }
    }
}

#[test]
fn extracted_docx_preserves_tabs_and_pdf_page_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pages.docx");
    native::extracted_document("first\tcell\n\u{c}second page", "docx", &path).unwrap();
    let text = native::document_text(&path).unwrap();
    assert!(text.contains("first\tcell"));
    assert_eq!(text.matches('\u{c}').count(), 1);
    assert!(text.contains("second page"));
}

#[test]
fn word_packages_reject_invalid_xml_and_unsafe_paths_without_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml"/></Types>"#;
    for (index, document, extra) in [
        (0,b"invalid XML".as_slice(), "safe.xml"),
        (1,b"<wrong/>".as_slice(), "safe.xml"),
        (2,br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#.as_slice(), "../outside.xml"),
    ] {
        let source = dir.path().join(format!("bad-{index}.dotx"));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
        for (name, bytes) in [("[Content_Types].xml",types.as_slice()),("word/document.xml",document),(extra,b"unused".as_slice())] {
            zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        let original = std::fs::read(&source).unwrap();
        let report = run(&source,"docx");
        assert_eq!(report.errors().len(),1);
        assert!(report.outputs().is_empty());
        assert_eq!(std::fs::read(source).unwrap(),original);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}

fn run(path: &Path, target: &str) -> Report {
    let plan = Plan::build(
        &[path.to_path_buf()],
        target,
        Settings::default(),
        Tools::default(),
    )
    .unwrap();
    execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {})
}

fn pcm_wave(bits: u16, channels: u16, rate: u32, samples: &[u8]) -> Vec<u8> {
    let mut wave = Vec::new();
    let length = samples.len() as u32;
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&(36 + length + length % 2).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0");
    wave.extend_from_slice(&channels.to_le_bytes());
    wave.extend_from_slice(&rate.to_le_bytes());
    wave.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits / 8)).to_le_bytes());
    wave.extend_from_slice(&(channels * (bits / 8)).to_le_bytes());
    wave.extend_from_slice(&bits.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&length.to_le_bytes());
    wave.extend_from_slice(samples);
    if length % 2 != 0 {
        wave.push(0);
    }
    wave
}

#[test]
fn small_opus_engine_has_explicit_missing_state_and_preserves_native_priority() {
    let source = Path::new("song.wav");
    let opus = FORMATS
        .iter()
        .find(|format| format.extension == "opus")
        .unwrap();
    let flac = FORMATS
        .iter()
        .find(|format| format.extension == "flac")
        .unwrap();
    let mut tools = Tools::default();
    assert_eq!(
        catalog::route(source, opus, &tools),
        Some(Backend::External(Tool::Opus))
    );
    let option = options(&[source.to_path_buf()], &tools)
        .into_iter()
        .find(|option| option.target.extension == "opus")
        .unwrap();
    assert_eq!(option.missing, [Tool::Opus]);
    tools.0.insert(Tool::Ffmpeg, PathBuf::from("ffmpeg"));
    assert_eq!(
        catalog::route(source, opus, &tools),
        Some(Backend::External(Tool::Ffmpeg))
    );
    tools.0.insert(Tool::Opus, PathBuf::from("opusenc"));
    assert_eq!(
        catalog::route(source, opus, &tools),
        Some(Backend::External(Tool::Opus))
    );
    assert_eq!(catalog::route(source, flac, &tools), Some(Backend::Flac));
    assert_eq!(
        catalog::route(Path::new("song.ape"), opus, &tools),
        Some(Backend::External(Tool::Ffmpeg))
    );
}

#[test]
fn native_pcm_preserves_samples_channels_rates_and_odd_chunk_padding() {
    let dir = tempfile::tempdir().unwrap();
    for bits in [8, 16, 24, 32] {
        for channels in [1, 2, 6] {
            let source = dir.path().join(format!("{bits}-{channels}.wav"));
            let samples: Vec<u8> = (0..(bits / 8 * channels * 3))
                .map(|i| (i * 37) as u8)
                .collect();
            let original = pcm_wave(bits, channels, 44100, &samples);
            std::fs::write(&source, &original).unwrap();
            for target in ["aiff", "au"] {
                let converted = run(&source, target);
                assert!(converted.errors().is_empty(), "{:?}", converted.errors());
                let path = converted.outputs()[0].clone();
                let bytes = std::fs::read(&path).unwrap();
                let (data_offset, channels_offset, bits_offset) = if target == "aiff" {
                    (54, 20, 26)
                } else {
                    (24, 20, 0)
                };
                if target == "aiff" {
                    assert_eq!(&bytes[28..38], &[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
                    assert_eq!(
                        u16::from_be_bytes(
                            bytes[channels_offset..channels_offset + 2]
                                .try_into()
                                .unwrap()
                        ),
                        channels
                    );
                    assert_eq!(
                        u16::from_be_bytes(bytes[bits_offset..bits_offset + 2].try_into().unwrap()),
                        bits
                    );
                } else {
                    assert_eq!(
                        u32::from_be_bytes(
                            bytes[channels_offset..channels_offset + 4]
                                .try_into()
                                .unwrap()
                        ),
                        u32::from(channels)
                    );
                    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 44100);
                }
                let expected: Vec<_> = if bits == 8 {
                    samples.iter().map(|s| s ^ 0x80).collect()
                } else {
                    samples
                        .chunks_exact(usize::from(bits / 8))
                        .flat_map(|s| s.iter().rev().copied())
                        .collect()
                };
                assert_eq!(&bytes[data_offset..data_offset + samples.len()], expected);
                let back = run(&path, "wav");
                assert!(back.errors().is_empty(), "{:?}", back.errors());
                assert_eq!(std::fs::read(&back.outputs()[0]).unwrap(), original);
            }
            assert_eq!(std::fs::read(source).unwrap(), original);
        }
    }
}

#[test]
fn native_pcm_rejects_truncation_partial_frames_and_compressed_wave_without_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let valid = pcm_wave(16, 2, 48000, &[0; 16]);
    let mut compressed = valid.clone();
    compressed[20] = 3; // IEEE float is deliberately not interpreted as integer PCM.
    let partial = pcm_wave(16, 2, 48000, &[0; 3]);
    for (index, bytes) in [valid[..45].to_vec(), compressed, partial, vec![0; 100]]
        .iter()
        .enumerate()
    {
        let path = dir.path().join(format!("invalid-{index}.wav"));
        std::fs::write(&path, bytes).unwrap();
        let report = run(&path, "aiff");
        assert!(report.outputs().is_empty());
        assert_eq!(report.errors().len(), 1);
        assert_eq!(std::fs::read(path).unwrap(), *bytes);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}

#[test]
fn independent_ffmpeg_reader_decodes_native_aiff_and_au_when_installed() {
    let tools = Tools::discover();
    let Some(ffmpeg) = tools.get(Tool::Ffmpeg) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("samples.wav");
    let samples: Vec<_> = [-32768_i16, -1024, -1, 0, 1, 1024, 32767]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect();
    std::fs::write(&source, pcm_wave(16, 1, 44100, &samples)).unwrap();
    for target in ["aiff", "au"] {
        let report = run(&source, target);
        assert!(report.errors().is_empty(), "{:?}", report.errors());
        let decoded = std::process::Command::new(ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&report.outputs()[0])
            .args(["-f", "s16le", "-acodec", "pcm_s16le", "pipe:1"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert_eq!(decoded.stdout, samples);
    }
}

#[test]
fn handmade_flac_is_lossless_for_all_pcm_depths_channels_and_containers() {
    use md5::{Digest, Md5};
    let tools = Tools::discover();
    let Some(reader) = tools.get(Tool::Ffmpeg) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    for bits in [8_u16, 16, 24, 32] {
        for channels in [1_u16, 2, 6, 8] {
            let width = usize::from(bits / 8);
            let mut state = 0x12345678_u32;
            let mut samples = Vec::new();
            // Constant, predictable, noise and full-scale extremes, plus a short tail.
            for frame in 0..12291 {
                for channel in 0..channels {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    let value = match frame / 4096 {
                        0 => i32::from(channel) * 16,
                        1 => (frame % 128 - 64) * (i32::from(channel) + 1),
                        2 => state as i32,
                        _ => [i32::MIN, i32::MAX, 0][(frame % 3) as usize],
                    };
                    let mut bytes = value.to_le_bytes();
                    if bits == 8 {
                        bytes[0] ^= 0x80;
                    }
                    samples.extend_from_slice(&bytes[..width]);
                }
            }
            let source = dir.path().join(format!("{bits}-{channels}.wav"));
            let original = pcm_wave(bits, channels, 44100, &samples);
            std::fs::write(&source, &original).unwrap();
            let mut expected = samples.clone();
            if bits == 8 {
                for byte in &mut expected {
                    *byte ^= 0x80;
                }
            }
            for container in ["wav", "aiff", "au"] {
                let input = if container == "wav" {
                    source.clone()
                } else {
                    let report = run(&source, container);
                    assert!(report.errors().is_empty(), "{:?}", report.errors());
                    report.outputs()[0].clone()
                };
                let plan =
                    Plan::build(&[input], "flac", Settings::default(), Tools::default()).unwrap();
                assert_eq!(plan.commands[0].backend, Backend::Flac);
                let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
                assert!(report.errors().is_empty(), "{:?}", report.errors());
                let path = &report.outputs()[0];
                let bytes = std::fs::read(path).unwrap();
                assert_eq!(&bytes[..8], b"fLaC\x80\0\0\x22");
                assert_eq!(&bytes[26..42], Md5::digest(&expected).as_slice());
                let raw = match bits {
                    8 => "s8",
                    16 => "s16le",
                    24 => "s24le",
                    _ => "s32le",
                };
                let codec = format!("pcm_{raw}");
                let decoded = std::process::Command::new(reader)
                    .args(["-v", "error", "-xerror", "-err_detect", "explode", "-i"])
                    .arg(path)
                    .args(["-f", raw, "-acodec", &codec, "pipe:1"])
                    .output()
                    .unwrap();
                assert!(
                    decoded.status.success(),
                    "{bits}/{channels}/{container}: {}",
                    String::from_utf8_lossy(&decoded.stderr)
                );
                assert_eq!(decoded.stdout, expected, "{bits}/{channels}/{container}");
            }
            assert_eq!(std::fs::read(source).unwrap(), original);
        }
    }
}

#[test]
fn handmade_flac_compresses_tones_silence_short_tails_and_multibyte_frame_numbers() {
    let tools = Tools::discover();
    let dir = tempfile::tempdir().unwrap();
    for (name, frames) in [("silence", 4096 * 130 + 1), ("tone", 44107), ("one", 1)] {
        let samples: Vec<_> = (0..frames)
            .flat_map(|index| {
                let sample = if name == "silence" {
                    0
                } else {
                    ((index as f64 * 440.0 * std::f64::consts::TAU / 44100.0).sin() * 16000.0)
                        as i16
                };
                sample.to_le_bytes()
            })
            .collect();
        let source = dir.path().join(format!("{name}.wav"));
        std::fs::write(&source, pcm_wave(16, 1, 44100, &samples)).unwrap();
        let report = run(&source, "flac");
        assert!(report.errors().is_empty(), "{:?}", report.errors());
        let output = &report.outputs()[0];
        if frames > 1 {
            assert!(std::fs::metadata(output).unwrap().len() < (samples.len() / 2) as u64);
        }
        if let Some(reader) = tools.get(Tool::Ffmpeg) {
            let decoded = std::process::Command::new(reader)
                .args(["-v", "error", "-xerror", "-i"])
                .arg(output)
                .args(["-f", "s16le", "-acodec", "pcm_s16le", "pipe:1"])
                .output()
                .unwrap();
            assert!(
                decoded.status.success(),
                "{}",
                String::from_utf8_lossy(&decoded.stderr)
            );
            assert_eq!(decoded.stdout, samples);
        }
    }
}

#[test]
fn handmade_flac_rejects_bad_pcm_and_excess_channels_and_cleans_up_on_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("bad", vec![0; 100]),
        ("channels", pcm_wave(16, 9, 44100, &[0; 18])),
        ("partial", pcm_wave(24, 2, 44100, &[0; 5])),
    ] {
        let source = dir.path().join(format!("{name}.wav"));
        std::fs::write(&source, &bytes).unwrap();
        let report = run(&source, "flac");
        assert!(report.outputs().is_empty());
        assert_eq!(report.errors().len(), 1);
    }
    let source = dir.path().join("cancel.wav");
    std::fs::write(&source, pcm_wave(16, 1, 44100, &[0; 32768])).unwrap();
    let plan = Plan::build(&[source], "flac", Settings::default(), Tools::default()).unwrap();
    let cancel = AtomicBool::new(true);
    assert!(execute_one(&plan.commands[0], &plan, &cancel).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}

#[test]
fn subtitle_pipeline_converts_without_engines_and_keeps_originals() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("subtítulos.srt");
    let source = "1\r\n00:00:01,250 --> 00:00:03,500\r\nHola &amp; adiós\r\nAna\r\n\r\n2\r\n00:00:04,000 --> 00:00:05,000\r\n&lt;literal&gt;\r\n";
    std::fs::write(&path, source).unwrap();
    for target in ["vtt", "ass", "smi", "ttml", "lrc", "txt"] {
        let plan = Plan::build(
            &[path.clone()],
            target,
            Settings::default(),
            Tools::default(),
        )
        .unwrap();
        assert!(matches!(plan.commands[0].backend, Backend::Subtitles));
        let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
        assert!(
            report.errors().is_empty(),
            "{target}: {:?}",
            report.errors()
        );
        if target != "txt" {
            let back = run(&report.outputs()[0], "srt");
            assert!(back.errors().is_empty(), "{target}: {:?}", back.errors());
            let result = std::fs::read_to_string(&back.outputs()[0]).unwrap();
            assert!(
                result.contains("00:00:01,250 --> 00:00:03,500"),
                "{target}: {result}"
            );
            assert!(result.contains("Hola &amp; adiós"), "{target}: {result}");
            assert!(result.contains("&lt;literal&gt;"), "{target}: {result}");
        }
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
fn invalid_subtitle_intervals_are_not_published() {
    let dir = tempfile::tempdir().unwrap();
    for (index, body) in [
        "1\n00:00:03,000 --> 00:00:01,000\nwrong",
        "not a subtitle",
        "1\n00:01:99,000 --> 00:02:00,000\nwrong",
    ]
    .iter()
    .enumerate()
    {
        let source = dir.path().join(format!("bad-{index}.srt"));
        std::fs::write(&source, body).unwrap();
        assert_eq!(run(&source, "vtt").errors().len(), 1);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[test]
fn independent_ffmpeg_reader_checks_native_ass_and_webvtt_when_installed() {
    let tools = Tools::discover();
    let Some(ffmpeg) = tools.get(Tool::Ffmpeg) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("text.srt");
    std::fs::write(
        &source,
        "1\n00:00:01,250 --> 00:00:03,500\nAna: \"Hola\" &amp; José\n{literal} C:\\Windows\n\n",
    )
    .unwrap();
    for target in ["vtt", "ass"] {
        let report = run(&source, target);
        assert!(report.errors().is_empty(), "{:?}", report.errors());
        let decoded = std::process::Command::new(ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&report.outputs()[0])
            .args(["-f", "srt", "pipe:1"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let text = String::from_utf8(decoded.stdout).unwrap();
        assert!(
            text.contains("00:00:01,250 --> 00:00:03,500"),
            "{target}: {text}"
        );
        assert!(text.contains("\"Hola\""), "{target}: {text}");
        assert!(text.contains("José"), "{target}: {text}");
        // FFmpeg protects literal braces with ASS-style escapes in SRT output.
        // Newer versions also insert WORD JOINER after a literal backslash to
        // keep it from being interpreted as an ASS control sequence.
        let literal = text
            .replace("\\{", "{")
            .replace("\\}", "}")
            .replace("\\\u{2060}", "\\");
        assert!(
            literal.contains("{literal} C:\\Windows"),
            "{target}: {text}"
        );
    }
}

#[test]
fn native_svg_rasterization_unpremultiplies_alpha_and_bounds_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("vector.svg");
    let xml = "<svg xmlns='http://www.w3.org/2000/svg' width='20' height='10'><rect width='20' height='10' fill='#ff0000' fill-opacity='0.5'/></svg>";
    std::fs::write(&source, xml).unwrap();
    let png = run(&source, "png");
    assert!(png.errors().is_empty(), "{:?}", png.errors());
    let image = image::open(&png.outputs()[0]).unwrap().to_rgba8();
    assert_eq!(image.dimensions(), (20, 10));
    assert_eq!(image.get_pixel(4, 4).0, [255, 0, 0, 128]);
    let gzip = run(&source, "svgz");
    assert!(gzip.errors().is_empty(), "{:?}", gzip.errors());
    let restored = run(&gzip.outputs()[0], "svg");
    assert!(restored.errors().is_empty(), "{:?}", restored.errors());
    assert_eq!(
        std::fs::read_to_string(&restored.outputs()[0]).unwrap(),
        xml
    );
    let plan = Plan::build(
        &[source],
        "png",
        Settings {
            max_edge: Some(5),
            ..Settings::default()
        },
        Tools::default(),
    )
    .unwrap();
    let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    assert_eq!(image::open(&report.outputs()[0]).unwrap().width(), 5);
}

#[test]
fn native_svg_refuses_external_images_and_decompression_bombs() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("external.svg");
    std::fs::write(&source, "<svg xmlns='http://www.w3.org/2000/svg' width='20' height='10'><image href='file:///private/image.png' width='20' height='10'/></svg>").unwrap();
    assert_eq!(run(&source, "png").errors().len(), 1);
    let source = dir.path().join("large.svgz");
    let mut gzip =
        flate2::write::GzEncoder::new(File::create(&source).unwrap(), flate2::Compression::fast());
    let block = [b' '; 1024 * 1024];
    for _ in 0..65 {
        gzip.write_all(&block).unwrap();
    }
    gzip.finish().unwrap();
    assert_eq!(run(&source, "svg").errors().len(), 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn contacts_vcard_csv_tsv_and_json_roundtrip_multiple_values_without_an_engine() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("contactos.vcf");
    let input = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ana Pérez\r\nN:Pérez;Ana;;;\r\nEMAIL:ana@example.com\r\nEMAIL:work@example.com\r\nTEL:+521234\r\nNOTE:Primera\\nSegunda línea\r\nADR:;;Calle 1;Ciudad;Región;123;México\r\nEND:VCARD\r\n";
    std::fs::write(&source, input).unwrap();
    for target in ["csv", "tsv", "json"] {
        let report = run(&source, target);
        assert!(
            report.errors().is_empty(),
            "{target}: {:?}",
            report.errors()
        );
        let back = run(&report.outputs()[0], "vcf");
        assert!(back.errors().is_empty(), "{target}: {:?}", back.errors());
        let json = run(&back.outputs()[0], "json");
        assert!(json.errors().is_empty(), "{:?}", json.errors());
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&json.outputs()[0]).unwrap()).unwrap();
        assert_eq!(
            data[0]["emails"],
            serde_json::json!(["ana@example.com", "work@example.com"])
        );
        assert_eq!(data[0]["note"], "Primera\nSegunda línea");
        assert_eq!(data[0]["addresses"][0][2], "Calle 1");
    }
    assert_eq!(std::fs::read_to_string(source).unwrap(), input);
    let csv = dir.path().join("simple.csv");
    std::fs::write(
        &csv,
        "First Name,Last Name,Email,Phone\r\nAna,Pérez,ana@example.com,+521234\r\n",
    )
    .unwrap();
    let report = run(&csv, "vcf");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    assert!(std::fs::read_to_string(&report.outputs()[0])
        .unwrap()
        .contains("FN:Ana Pérez\r\n"));
}

#[test]
fn native_books_write_valid_epub_packages_and_roundtrip_text_without_engines() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Libro de María.txt");
    std::fs::write(&source, "Capítulo uno\nA < B & C\nOtra línea\n").unwrap();
    let report = run(&source, "epub");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    let epub = report.outputs()[0].clone();
    let mut zip = zip::ZipArchive::new(File::open(&epub).unwrap()).unwrap();
    let first = zip.by_index(0).unwrap();
    assert_eq!(first.name().unwrap(), "mimetype");
    assert_eq!(first.compression(), zip::CompressionMethod::Stored);
    drop(first);
    for index in 1..zip.len() {
        let mut text = String::new();
        zip.by_index(index)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert!(roxmltree::Document::parse(&text).is_ok(), "{text}");
    }
    let report = run(&epub, "fb2");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    let report = run(&report.outputs()[0], "docx");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    let report = run(&report.outputs()[0], "txt");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    assert_eq!(
        std::fs::read_to_string(&report.outputs()[0]).unwrap(),
        "Capítulo uno\nA < B & C\nOtra línea\n"
    );
}

fn spine_epub(path: &Path, encrypted: bool) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    let mut entries = vec![
        ("META-INF/container.xml", "<container><rootfiles><rootfile full-path='OEBPS/package.opf'/></rootfiles></container>"),
        ("OEBPS/package.opf", "<package><metadata><title>Book</title></metadata><manifest><item id='a' href='../Text/second.xhtml' media-type='application/xhtml+xml'/><item id='b' href='../Text/first.xhtml' media-type='application/xhtml+xml'/></manifest><spine><itemref idref='b'/><itemref idref='a'/></spine></package>"),
        ("Text/second.xhtml", "<html><head><title>not body</title><script>not script</script></head><body><p>Second <b>chapter</b></p><script>not script</script></body></html>"),
        ("Text/first.xhtml", "<html><body><h1>First</h1><p>one<br/>two</p></body></html>"),
    ];
    if encrypted {
        entries.push(("META-INF/encryption.xml", "<encryption><EncryptedData><CipherReference URI='Text/first.xhtml'/></EncryptedData></encryption>"));
    }
    for (name, text) in entries {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn epub_uses_spine_order_and_refuses_encrypted_content() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("ordered.epub");
    spine_epub(&source, false);
    let report = run(&source, "txt");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    assert_eq!(
        std::fs::read_to_string(&report.outputs()[0]).unwrap(),
        "First\none\ntwo\n\nSecond chapter"
    );
    let encrypted = dir.path().join("encrypted.epub");
    spine_epub(&encrypted, true);
    let report = run(&encrypted, "txt");
    assert!(report.outputs().is_empty());
    assert!(report.errors()[0].contains("Encrypted EPUB"));
}

#[cfg(target_os = "windows")]
#[test]
fn windows_audio_encodes_and_decodes_without_external_tools() {
    let mut tools = Tools::discover();
    let independent_reader = tools.get(Tool::Ffmpeg).map(Path::to_path_buf);
    tools.0.clear();
    assert!(
        tools.1.wave,
        "Media Foundation is required on this validation machine"
    );
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("tone.wav");
    let samples: Vec<_> = (0..22050)
        .flat_map(|i| {
            ((f64::from(i) * 440.0 * std::f64::consts::TAU / 44100.0)
                .sin()
                .mul_add(16000.0, 0.0) as i16)
                .to_le_bytes()
        })
        .collect();
    std::fs::write(&source, pcm_wave(16, 1, 44100, &samples)).unwrap();
    for target in ["mp3", "m4a", "aac"] {
        assert!(
            tools.1.supports("wav", target),
            "Missing system {target} encoder"
        );
        let plan = Plan::build(
            &[source.clone()],
            target,
            Settings::default(),
            tools.clone(),
        )
        .unwrap();
        assert!(matches!(plan.commands[0].backend, Backend::SystemAudio));
        let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
        assert!(
            report.errors().is_empty(),
            "{target}: {:?}",
            report.errors()
        );
        let encoded = report.outputs()[0].clone();
        assert!(std::fs::metadata(&encoded).unwrap().len() > 500);
        if let Some(reader) = &independent_reader {
            let decoded = std::process::Command::new(reader)
                .args(["-v", "error", "-i"])
                .arg(&encoded)
                .args(["-f", "f32le", "-acodec", "pcm_f32le", "pipe:1"])
                .output()
                .unwrap();
            assert!(
                decoded.status.success(),
                "{}",
                String::from_utf8_lossy(&decoded.stderr)
            );
            let values: Vec<_> = decoded
                .stdout
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            assert!(values.len() > 20000 && values.len() < 40000);
            assert!(values.iter().all(|v| v.is_finite()));
            let energy =
                values.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / values.len() as f64;
            assert!(
                energy > 0.02 && energy < 0.3,
                "{target}: unexpected decoded signal energy {energy}"
            );
        }
        for target in ["flac", "aiff", "au"] {
            let plan = Plan::build(
                &[encoded.clone()],
                target,
                Settings::default(),
                tools.clone(),
            )
            .unwrap();
            assert_eq!(plan.commands[0].backend, Backend::SystemAudio);
            let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
            assert!(
                report.errors().is_empty(),
                "{target}: {:?}",
                report.errors()
            );
            if let Some(reader) = &independent_reader {
                let decoded = std::process::Command::new(reader)
                    .args(["-v", "error", "-xerror", "-i"])
                    .arg(&report.outputs()[0])
                    .args(["-f", "s16le", "-acodec", "pcm_s16le", "pipe:1"])
                    .output()
                    .unwrap();
                assert!(
                    decoded.status.success(),
                    "{target}: {}",
                    String::from_utf8_lossy(&decoded.stderr)
                );
                assert!(decoded.stdout.len() > 40000);
            }
        }
        let plan = Plan::build(&[encoded], "wav", Settings::default(), tools.clone()).unwrap();
        let decoded = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
        assert!(
            decoded.errors().is_empty(),
            "{target}: {:?}",
            decoded.errors()
        );
        let bytes = std::fs::read(&decoded.outputs()[0]).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert!(
            bytes.len() > 40000 && bytes.len() < 80000,
            "{target}: unexpected decoded audio size {}",
            bytes.len()
        );
    }
}

#[test]
fn converts_actual_image_bytes_and_preserves_source_and_colliding_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("foto con ñ.png");
    image::RgbaImage::from_pixel(9, 6, image::Rgba([255, 0, 0, 0]))
        .save(&source)
        .unwrap();
    let original = std::fs::read(&source).unwrap();
    let existing = dir.path().join("foto con ñ.jpg");
    std::fs::write(&existing, b"do not overwrite").unwrap();
    let report = run(&source, "jpg");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    let outputs = report.outputs();
    assert_eq!(outputs, [dir.path().join("foto con ñ (1).jpg")]);
    let img = image::open(&outputs[0]).unwrap().to_rgb8();
    assert_eq!(img.dimensions(), (9, 6));
    assert!(img.get_pixel(0, 0).0.iter().all(|c| *c > 245));
    assert_eq!(std::fs::read(source).unwrap(), original);
    assert_eq!(std::fs::read(existing).unwrap(), b"do not overwrite");
}

#[test]
fn native_image_outputs_are_decodable_and_resize_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.png");
    image::RgbaImage::from_pixel(40, 20, image::Rgba([30, 60, 90, 255]))
        .save(&source)
        .unwrap();
    let bitmap = dir.path().join("input.bmp");
    image::RgbImage::from_pixel(8, 4, image::Rgb([30, 60, 90]))
        .save(&bitmap)
        .unwrap();
    let png = run(&bitmap, "png");
    assert!(png.errors().is_empty(), "{:?}", png.errors());
    assert_eq!(image::open(&png.outputs()[0]).unwrap().width(), 8);
    for target in ["jpg", "webp", "bmp", "tiff", "gif", "ico"] {
        let plan = Plan::build(
            &[source.clone()],
            target,
            Settings {
                max_edge: Some(10),
                ..Settings::default()
            },
            Tools::default(),
        )
        .unwrap();
        let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
        assert!(
            report.errors().is_empty(),
            "{target}: {:?}",
            report.errors()
        );
        assert_eq!(
            (
                image::open(&report.outputs()[0]).unwrap().width(),
                image::open(&report.outputs()[0]).unwrap().height()
            ),
            (10, 5)
        );
    }
}

#[test]
#[cfg(not(feature = "free"))]
fn parallel_conversions_claim_distinct_names_in_a_shared_output_directory() {
    let dir = tempfile::tempdir().unwrap();
    let mut sources = Vec::new();
    for folder in ["a", "b"] {
        let parent = dir.path().join(folder);
        std::fs::create_dir(&parent).unwrap();
        let source = parent.join("same.png");
        image::RgbImage::new(2, 2).save(&source).unwrap();
        sources.push(source);
    }
    let plan = Plan::build(
        &sources,
        "bmp",
        Settings {
            output_dir: Some(dir.path().to_path_buf()),
            ..Settings::default()
        },
        Tools::default(),
    )
    .unwrap();
    let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
    let outputs = report.outputs();
    assert_eq!(outputs.len(), 2);
    assert_ne!(outputs[0], outputs[1]);
    assert!(outputs.iter().all(|p| image::open(p).is_ok()));
}

#[test]
fn malformed_input_leaves_no_partial_output_and_keeps_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("invalid.png");
    std::fs::write(&source, b"not an image").unwrap();
    assert_eq!(run(&source, "jpg").errors().len(), 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(std::fs::read(source).unwrap(), b"not an image");
}

#[test]
fn pre_cancelled_batch_does_not_create_any_files() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.json");
    std::fs::write(&source, "[1,2]").unwrap();
    let plan = Plan::build(&[source], "jsonl", Settings::default(), Tools::default()).unwrap();
    let report = execute(&plan, &Arc::new(AtomicBool::new(true)), |_, _, _| {});
    assert_eq!(report.cancelled(), 1);
    assert!(report.outputs().is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn quoted_unicode_tables_roundtrip_with_embedded_newlines_quotes_and_empty_cells() {
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("table.csv");
    std::fs::write(
        &csv,
        "name,note,empty\r\n\"María, Ana\",\"line 1\r\n\"\"quoted\"\"\",\r\n",
    )
    .unwrap();
    let json = run(&csv, "json").outputs().pop().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    assert_eq!(value[0]["name"], "María, Ana");
    assert_eq!(value[0]["note"], "line 1\r\n\"quoted\"");
    assert_eq!(value[0]["empty"], "");
    let tsv = run(&json, "tsv").outputs().pop().unwrap();
    let back = run(&tsv, "json").outputs().pop().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(back).unwrap()).unwrap(),
        value
    );
}

#[test]
fn json_lines_and_toml_use_real_serialization() {
    let dir = tempfile::tempdir().unwrap();
    let toml = dir.path().join("config.toml");
    std::fs::write(&toml, "title = 'Atlas'\n[settings]\nquality = 90\n").unwrap();
    let json = run(&toml, "json").outputs().pop().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    assert_eq!(value["settings"]["quality"], 90);
    let back = run(&json, "toml").outputs().pop().unwrap();
    assert!(std::fs::read_to_string(back)
        .unwrap()
        .contains("quality = 90"));
    let lines = dir.path().join("rows.jsonl");
    std::fs::write(&lines, "{\"id\":1}\n{\"id\":2}\n").unwrap();
    let json = run(&lines, "json").outputs().pop().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(json).unwrap())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn invalid_tables_and_non_tabular_json_fail_without_outputs() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body, target) in [
        ("broken.csv", "a,b\n\"unclosed,b", "json"),
        ("duplicates.csv", "a,a\n1,2", "json"),
        ("ragged.csv", "a,b\n1,2,3", "json"),
        ("scalar.json", "42", "csv"),
    ] {
        let source = dir.path().join(name);
        std::fs::write(&source, body).unwrap();
        assert_eq!(run(&source, target).errors().len(), 1, "{name}");
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}

#[test]
fn markdown_html_contains_real_markup_and_escapes_raw_html() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.md");
    std::fs::write(
        &path,
        "# Title\n\nA **bold** paragraph.\n\n<script>alert(1)</script>",
    )
    .unwrap();
    let html = run(&path, "html").outputs().pop().unwrap();
    let html = std::fs::read_to_string(html).unwrap();
    assert!(html.contains("<h1>Title</h1>"));
    assert!(html.contains("<strong>bold</strong>"));
    assert!(!html.contains("<script>"));
}

#[test]
fn native_archive_repack_preserves_contents_without_extracting() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("files.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
    zip.add_directory("nested/", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.start_file("nested/note.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"hello atlas").unwrap();
    zip.finish().unwrap();
    let tar = run(&source, "tar.gz");
    assert!(tar.errors().is_empty(), "{:?}", tar.errors());
    let tar = tar.outputs().pop().unwrap();
    let report = run(&tar, "zip");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    let mut zip = zip::ZipArchive::new(File::open(&report.outputs()[0]).unwrap()).unwrap();
    let mut contents = String::new();
    zip.by_name("nested/note.txt")
        .unwrap()
        .read_to_string(&mut contents)
        .unwrap();
    assert_eq!(contents, "hello atlas");
    assert!(!dir.path().join("nested").exists());
}

#[test]
fn archive_traversal_is_rejected_without_publishing_an_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("bad.zip");
    let mut zip = zip::ZipWriter::new(File::create(&source).unwrap());
    zip.start_file("../outside.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"bad").unwrap();
    zip.finish().unwrap();
    assert_eq!(run(&source, "tar").errors().len(), 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn batch_catalog_requires_every_input_to_be_compatible_and_exposes_missing_engines() {
    let tools = Tools::default();
    let options = options(&[PathBuf::from("a.heic"), PathBuf::from("b.docx")], &tools);
    assert!(options.iter().all(|o| o.target.extension == "pdf"));
    let image = options
        .iter()
        .find(|o| o.target.extension == "pdf")
        .unwrap();
    assert!(!image.available());
    assert!(image.missing.contains(&Tool::Magick));
    assert!(image.missing.contains(&Tool::Office));
    assert!(super::options(&[PathBuf::from("a.unknown")], &tools).is_empty());
    assert!(!super::options(&[PathBuf::from("a.gif")], &tools)
        .iter()
        .find(|o| o.target.extension == "png")
        .unwrap()
        .available());
}

#[test]
fn plan_rejects_directories_missing_engines_and_no_op_batches() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Plan::build(
        &[dir.path().to_path_buf()],
        "zip",
        Settings::default(),
        Tools::default()
    )
    .is_err());
    let source = dir.path().join("audio.mp3");
    std::fs::write(&source, b"placeholder").unwrap();
    assert!(Plan::build(
        &[source.clone()],
        "wav",
        Settings::default(),
        Tools::default()
    )
    .is_err());
    assert!(Plan::build(&[source], "mp3", Settings::default(), Tools::default()).is_err());
}

use std::fs::File;

#[test]
fn catalog_does_not_offer_unsupported_legacy_doc_or_calibre_html_routes() {
    let mut tools = Tools::default();
    for tool in Tool::ALL {
        tools.0.insert(tool, PathBuf::from(tool.executable()));
    }
    let legacy = options(&[PathBuf::from("legacy.doc")], &tools);
    assert!(!legacy
        .iter()
        .any(|o| ["md", "epub"].contains(&o.target.extension)));
    assert!(legacy.iter().any(|o| o.target.extension == "pdf"));
    assert!(!options(&[PathBuf::from("book.mobi")], &tools)
        .iter()
        .any(|o| o.target.extension == "html"));
    let epub = options(&[PathBuf::from("book.epub")], &tools);
    assert_eq!(
        epub.iter()
            .find(|o| o.target.extension == "html")
            .unwrap()
            .engines,
        ["Atlas"]
    );
}

#[test]
fn native_pdf_has_valid_cross_references_and_a_lossless_rgb_image_stream() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("photo.png");
    image::RgbImage::from_pixel(13, 7, image::Rgb([11, 22, 33]))
        .save(&source)
        .unwrap();
    let report = run(&source, "pdf");
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    let bytes = std::fs::read(&report.outputs()[0]).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.4"));
    let text = String::from_utf8_lossy(&bytes);
    let xref: usize = text
        .split("startxref\n")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(&bytes[xref..xref + 4], b"xref");
    assert!(text.contains("/Width 13 /Height 7"));
    assert!(text.contains("/Filter /FlateDecode"));
}

#[test]
fn docx_export_is_an_ooxml_package_with_escaped_text() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("text.docx");
    super::native::extracted_document("A < B & C\nHola, María", "docx", &output).unwrap();
    let mut zip = zip::ZipArchive::new(File::open(output).unwrap()).unwrap();
    assert!(zip.by_name("[Content_Types].xml").is_ok());
    assert!(zip.by_name("_rels/.rels").is_ok());
    let mut document = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut document)
        .unwrap();
    assert!(document.contains("A &lt; B &amp; C"));
    assert!(document.contains("Hola, María"));
}

#[test]
fn installed_ffmpeg_converts_real_audio_when_available() {
    let mut tools = Tools::discover();
    tools.1 = Default::default(); // This test exercises the optional engine explicitly.
    if !tools.has(Tool::Ffmpeg) {
        eprintln!("FFmpeg integration not run: engine not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("audio con ñ.wav");
    let samples = 8000_u32;
    let mut wav = File::create(&source).unwrap();
    wav.write_all(b"RIFF").unwrap();
    wav.write_all(&(36 + samples * 2).to_le_bytes()).unwrap();
    wav.write_all(b"WAVEfmt ").unwrap();
    wav.write_all(&16_u32.to_le_bytes()).unwrap();
    wav.write_all(&1_u16.to_le_bytes()).unwrap();
    wav.write_all(&1_u16.to_le_bytes()).unwrap();
    wav.write_all(&8000_u32.to_le_bytes()).unwrap();
    wav.write_all(&16000_u32.to_le_bytes()).unwrap();
    wav.write_all(&2_u16.to_le_bytes()).unwrap();
    wav.write_all(&16_u16.to_le_bytes()).unwrap();
    wav.write_all(b"data").unwrap();
    wav.write_all(&(samples * 2).to_le_bytes()).unwrap();
    wav.write_all(&vec![0; samples as usize * 2]).unwrap();
    drop(wav);
    let plan = Plan::build(&[source], "mp3", Settings::default(), tools).unwrap();
    let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    assert!(std::fs::metadata(&report.outputs()[0]).unwrap().len() > 100);
}

#[test]
fn installed_seven_zip_repacks_an_actual_archive_when_available() {
    let tools = Tools::discover();
    if !tools.has(Tool::SevenZip) {
        eprintln!("7-Zip integration not run: engine not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.zip");
    let mut zip = zip::ZipWriter::new(File::create(&source).unwrap());
    zip.start_file("note.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"7zip roundtrip").unwrap();
    zip.finish().unwrap();
    let plan = Plan::build(&[source], "7z", Settings::default(), tools.clone()).unwrap();
    let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    let plan = Plan::build(&report.outputs(), "zip", Settings::default(), tools).unwrap();
    let back = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
    assert!(back.errors().is_empty(), "{:?}", back.errors());
    let mut zip = zip::ZipArchive::new(File::open(&back.outputs()[0]).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name("note.txt")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "7zip roundtrip");
}

#[test]
fn installed_imagemagick_converts_extended_image_format_when_available() {
    let tools = Tools::discover();
    if !tools.has(Tool::Magick) {
        eprintln!("ImageMagick integration not run: engine not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("sample.png");
    image::RgbImage::from_pixel(12, 8, image::Rgb([50, 100, 150]))
        .save(&source)
        .unwrap();
    let plan = Plan::build(&[source], "avif", Settings::default(), tools).unwrap();
    let report = execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
    assert!(report.errors().is_empty(), "{:?}", report.errors());
    assert!(std::fs::read(&report.outputs()[0])
        .unwrap()
        .windows(4)
        .any(|w| w == b"avif"));
}

#[test]
fn native_text_and_docx_roundtrip_without_an_office_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("note.txt");
    std::fs::write(&source, "Primera línea\nA < B & C\n").unwrap();
    let docx = run(&source, "docx");
    assert!(docx.errors().is_empty(), "{:?}", docx.errors());
    let text = run(&docx.outputs()[0], "txt");
    assert!(text.errors().is_empty(), "{:?}", text.errors());
    assert_eq!(
        std::fs::read_to_string(&text.outputs()[0]).unwrap(),
        "Primera línea\nA < B & C\n"
    );
}

#[test]
fn wheel_offers_provider_installed_engines_only_when_installed() {
    let source = PathBuf::from("notes.rst");
    let mut tools = Tools::default();
    // Pandoc on PATH alone is not a provider package: still hidden.
    tools.0.insert(Tool::Pandoc, PathBuf::from("pandoc"));
    assert!(wheel_options(&[source.clone()], &tools, &[]).is_empty());
    let routes = wheel_options(&[source.clone()], &tools, &[Tool::Pandoc]);
    assert!(routes
        .iter()
        .any(|o| o.target.extension == "docx" && o.available()));
    assert!(routes.iter().all(|o| o.missing.is_empty()));
    // Not installed: no sector, but the provider is offered as locked.
    let empty = Tools::default();
    assert!(wheel_options(&[source.clone()], &empty, &[]).is_empty());
    assert_eq!(
        locked_engines(&[source.clone()], &empty, &[], &[Tool::Pandoc]),
        [Tool::Pandoc]
    );
    assert!(locked_engines(&[source], &empty, &[], &[]).is_empty());
    // Heavy engines without a provider never leak through the extra list.
    let mut all = Tools::default();
    for tool in Tool::ALL {
        all.0.insert(tool, PathBuf::from(tool.executable()));
    }
    for option in wheel_options(&[PathBuf::from("a.heic")], &all, &[Tool::Pandoc]) {
        assert!(!option.engines.contains(&"ImageMagick"));
    }
}

#[test]
fn managed_by_matches_provider_paths_and_portable_names() {
    let mut tools = Tools::default();
    tools
        .0
        .insert(Tool::Pandoc, PathBuf::from("C:/ext/pandoc.exe"));
    tools
        .0
        .insert(Tool::Ghostscript, PathBuf::from("C:/ext/gs.exe"));
    tools
        .0
        .insert(Tool::Ffmpeg, PathBuf::from("C:/path/ffmpeg.exe"));
    let managed: std::collections::HashMap<String, PathBuf> = [
        ("pandoc".to_string(), PathBuf::from("C:/ext/pandoc.exe")),
        ("gs".to_string(), PathBuf::from("C:/ext/gs.exe")),
        (
            "ffmpeg".to_string(),
            PathBuf::from("C:/ext/other-ffmpeg.exe"),
        ),
    ]
    .into_iter()
    .collect();
    let mut found = tools.managed_by(&managed);
    found.sort();
    assert_eq!(found, [Tool::Pandoc, Tool::Ghostscript]);
}
