#![allow(clippy::disallowed_methods)]
// reason: preview functions run on blocking workers and must touch files.

use std::path::Path;

use crate::ext::registry::CommandRef;
use atlas_model::preview_cache::{PreviewResult, SlidePreview};

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ExtPreview {
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        #[serde(default)]
        truncated: bool,
    },
    Document {
        #[serde(default)]
        title: String,
        blocks: Vec<String>,
        #[serde(default)]
        truncated: bool,
    },
    Presentation {
        slides: Vec<ExtSlide>,
        #[serde(default)]
        truncated: bool,
    },
    Error {
        message: String,
    },
}

#[derive(serde::Deserialize)]
struct ExtSlide {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: Vec<String>,
}

pub(crate) fn run_declarative_preview(cmd: &CommandRef, input: &Path) -> PreviewResult {
    use atlas_model::ext_model::PreviewOutput;
    let ext = match cmd.command.output {
        PreviewOutput::Image => "png",
        _ => "json",
    };
    let output =
        std::env::temp_dir().join(format!("atlas-preview-{}.{}", uuid::Uuid::new_v4(), ext));
    let argv = super::argv::build_thumb_argv(&cmd.command.run, input, &output);
    let Some((program, args)) = argv.split_first() else {
        return PreviewResult::Error("Preview extension has no command".into());
    };
    let Some(program) = super::approved_program(&cmd.tools, program) else {
        return PreviewResult::Error(format!("preview uses undeclared tool: {program}"));
    };
    let mut cmd_proc = std::process::Command::new(program);
    cmd_proc.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd_proc.creation_flags(crate::resources::tool_creation_flags(0x0800_0000));
    }
    let ran = cmd_proc.output();
    let result = match ran {
        Ok(out) if out.status.success() => match cmd.command.output {
            PreviewOutput::Image => super::img::image_file_to_preview(&output),
            _ => std::fs::read_to_string(&output)
                .map_err(|e| e.to_string())
                .and_then(|json| ext_preview_from_json(&json))
                .unwrap_or_else(PreviewResult::Error),
        },
        Ok(out) => PreviewResult::Error(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(e) => PreviewResult::Error(e.to_string()),
    };
    let _ = std::fs::remove_file(output);
    result
}

pub(crate) fn ext_preview_from_json(json: &str) -> Result<PreviewResult, String> {
    match serde_json::from_str::<ExtPreview>(json).map_err(|e| e.to_string())? {
        ExtPreview::Table {
            headers,
            rows,
            truncated,
        } => Ok(PreviewResult::Table {
            headers,
            rows,
            truncated,
        }),
        ExtPreview::Document {
            title,
            blocks,
            truncated,
        } => Ok(PreviewResult::Document {
            title,
            blocks,
            truncated,
        }),
        ExtPreview::Presentation { slides, truncated } => Ok(PreviewResult::Presentation {
            slides: slides
                .into_iter()
                .map(|s| SlidePreview {
                    title: s.title,
                    body: s.body,
                })
                .collect(),
            truncated,
        }),
        ExtPreview::Error { message } => Ok(PreviewResult::Error(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_preview_json_becomes_error() {
        assert!(ext_preview_from_json("{ nope").is_err());
    }
}
