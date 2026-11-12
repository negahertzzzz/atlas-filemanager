use std::collections::HashMap;
use std::path::{Path, PathBuf};

// reason: "{dir}"/"{stem}"/"{ext}" are literal template placeholders for .replace(),
// not format-macro args (they happen to match local variable names).
#[allow(clippy::literal_string_with_formatting_args)]
pub(crate) fn subst_token(token: &str, path: &Path, answers: &HashMap<String, String>) -> String {
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = token
        .replace("{input}", &path.to_string_lossy())
        .replace("{dir}", &dir)
        .replace("{stem}", &stem)
        .replace("{ext}", &ext)
        .replace("{output}", &format!("{dir}/{stem}_out.{ext}"));
    for (name, value) in answers {
        out = out.replace(&format!("{{prompt:{name}}}"), value);
    }
    out
}

pub(crate) fn build_argv(
    run: &str,
    inputs: &[PathBuf],
    answers: &HashMap<String, String>,
) -> Vec<String> {
    let primary = inputs.first().cloned().unwrap_or_default();
    let mut argv = Vec::new();
    for token in run.split_whitespace() {
        if token == "{input}" && inputs.len() > 1 {
            for p in inputs {
                argv.push(p.to_string_lossy().into_owned());
            }
        } else {
            argv.push(subst_token(token, &primary, answers));
        }
    }
    argv
}

// reason: literal template placeholders for .replace(), not format-macro args.
#[allow(clippy::literal_string_with_formatting_args)]
pub(crate) fn build_thumb_argv(run: &str, input: &Path, output: &Path) -> Vec<String> {
    let dir = input
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = input
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let output_prefix = output.with_extension("").to_string_lossy().into_owned();
    run.split_whitespace()
        .map(|token| {
            token
                .replace("{input}", &input.to_string_lossy())
                .replace("{output_prefix}", &output_prefix)
                .replace("{output}", &output.to_string_lossy())
                .replace("{dir}", &dir)
                .replace("{stem}", &stem)
                .replace("{ext}", &ext)
        })
        .collect()
}
