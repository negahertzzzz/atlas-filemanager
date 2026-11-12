//! Extension domain model.
//!
//! These structs describe installed extensions and runnable commands. Discovery,
//! filesystem scans and process execution live in `crate::ext`.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// How an extension's commands are executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtKind {
    /// Commands are shell-command templates run on the selection. No plugin code.
    #[default]
    Declarative,
    /// Virtual locations are provided by a long-lived JSON-lines sidecar.
    Sidecar,
    /// Implemented in-app for tightly integrated features.
    Builtin,
    /// Hidden dependency package that contributes portable tools for commands.
    #[serde(rename = "tool_provider")]
    ToolProvider,
}

/// Optional process for `kind = "sidecar"`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Sidecar {
    /// Binary path relative to the extension directory.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// One-shot request sent to a command-capable sidecar over stdin.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct SidecarCommandRequest {
    pub version: u32,
    pub command_id: String,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub answers: HashMap<String, String>,
    pub data_dir: PathBuf,
}

/// One-shot result emitted by a command-capable sidecar on stdout.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct SidecarCommandResponse {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub changed_dirs: Vec<PathBuf>,
}

/// A top-level section contributed to the sidebar by an extension.
#[derive(Debug, Clone, Deserialize)]
pub struct SidebarContribution {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub add_action: Option<String>,
}

/// What a command accepts as input from the current selection.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Accepts {
    /// File extensions (lowercase, no dot) the command applies to.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Applies to directories.
    #[serde(default)]
    pub dirs: bool,
    /// Applies to anything (overrides the filters above).
    #[serde(default)]
    pub any: bool,
}

impl Accepts {
    /// Does this command apply to a path with the given extension / dir-ness?
    pub fn matches(&self, ext: Option<&str>, is_dir: bool) -> bool {
        if self.any {
            return true;
        }
        if is_dir {
            return self.dirs;
        }
        match ext {
            Some(e) => self.extensions.iter().any(|x| x.eq_ignore_ascii_case(e)),
            None => false,
        }
    }
}

/// A single value prompted from the user before running a command, substituted
/// into the template as `{prompt:NAME}`.
#[derive(Debug, Clone, Deserialize)]
pub struct Prompt {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub default: String,
}

const fn default_true() -> bool {
    true
}

/// One contributed command (palette + context-menu entry).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtCommand {
    pub id: String,
    #[allow(dead_code)] // reason: extension manager and palette keep this field available
    pub title: String,
    /// Whether this command appears in file context menus. Palette access is independent.
    #[serde(default = "default_true")]
    pub context_menu: bool,
    #[serde(default)]
    pub accepts: Accepts,
    /// Declarative template. Placeholders: `{input}` `{dir}` `{stem}` `{ext}`
    /// `{output}` (= `{dir}/{stem}_out.{ext}`) and `{prompt:NAME}`.
    #[serde(default)]
    pub run: String,
    /// External binaries that must be on `PATH` for this command to be offered.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Run the template once per selected file (true) or once for the whole set.
    #[serde(default = "default_true")]
    pub per_file: bool,
    #[serde(default)]
    pub prompts: Vec<Prompt>,
    /// When true, this command is a *thumbnailer*: atlas runs it with `{input}` and a
    /// temp `{output}` PNG to generate a grid thumbnail for files it `accepts` (e.g.
    /// video/3D/psd via ffmpeg/ImageMagick). Not shown in the context menu.
    #[serde(default)]
    pub thumbnail: bool,
    /// When true, this command contributes an inspector preview for files it accepts.
    /// Preview commands are background-only and are not shown in command menus.
    #[serde(default)]
    pub preview: bool,
    /// When true, the command also appears as a sector in the Actions ring for
    /// selections it accepts and whose required tools are installed.
    #[serde(default)]
    pub ring: bool,
    /// Preview output format. Declarative previews write this to `{output}`; WASM
    /// previews return the same JSON shape directly.
    #[serde(default)]
    pub output: PreviewOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewOutput {
    #[default]
    Json,
    Image,
    Table,
    Text,
    Presentation,
}

/// A parsed `extension.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[allow(dead_code)] // reason: extension manager keeps this field available
    pub name: String,
    #[serde(default)]
    #[allow(dead_code)] // reason: extension manager keeps this field available
    pub version: String,
    #[serde(default)]
    #[allow(dead_code)] // reason: extension manager keeps this field available
    pub description: String,
    /// Official bundled extensions use "Atlas Team"; retirement checks it.
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub kind: ExtKind,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    #[allow(dead_code)]
    pub sidecar: Option<Sidecar>,
    #[serde(default)]
    pub sidebar: Vec<SidebarContribution>,
    #[serde(default)]
    pub commands: Vec<ExtCommand>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub download_url: String,
    #[serde(default)]
    pub archive_path: String,
    #[serde(default)]
    pub archive_format: String,
    #[serde(default)]
    pub signature_url: String,
    #[serde(default)]
    pub sha256: String,
    /// Optional exact download size, checked as well as SHA-256.
    #[serde(default)]
    pub download_bytes: u64,
    /// Uncompressed payload bytes, disclosed before downloading when known.
    #[serde(default)]
    pub installed_bytes: u64,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub license_url: String,
    /// Extract this archive directory as one package, including data/licenses.
    /// Empty retains the legacy executable-directory selection.
    #[serde(default)]
    pub archive_root: String,
    /// Optional package-relative allowlist for a reduced payload.
    #[serde(default)]
    pub archive_files: Vec<String>,
}

/// Validate an extension identifier (SEC-01).
///
/// Extension IDs must strictly contain only ASCII lowercase alphanumeric characters,
/// hyphens, or underscores (`[a-z0-9_-]+`). Any ID containing `..`, `/`, `\`, or control
/// characters is immediately rejected to prevent path traversal attacks.
pub fn validate_extension_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() {
        anyhow::bail!("Extension ID cannot be empty");
    }
    if id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.chars().any(|c| c.is_control())
    {
        anyhow::bail!("Extension ID contains invalid or traversal characters: {id:?}");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        anyhow::bail!("Extension ID must only contain [a-z0-9_-]+: {id:?}");
    }
    Ok(())
}

/// Convenience check returning true if `id` is a valid extension ID.
pub fn is_valid_extension_id(id: &str) -> bool {
    validate_extension_id(id).is_ok()
}

impl Manifest {
    pub fn parse(toml_str: &str) -> anyhow::Result<Self> {
        let manifest: Self = toml::from_str(toml_str)?;
        validate_extension_id(&manifest.id)?;
        Ok(manifest)
    }
}

// ── Marketplace / remote registry ────────────────────────────────────────────

/// Metadata for a single extension published in the remote R2 marketplace.
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteExtension {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub icon_url: Option<String>,
    pub latest_version: String,
    pub download_url: String,
    #[serde(default)]
    pub sha256: String,
}

/// The `registry.json` document served from the R2 bucket.
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteRegistry {
    pub last_updated: String,
    pub extensions: HashMap<String, RemoteExtension>,
}

// ── Installed command resolution ──────────────────────────────────────────────

/// A command resolved for the current selection, ready to run.
#[derive(Debug, Clone)]
pub struct CommandRef {
    pub ext_id: String,
    /// Extension directory + kind — used by the later-phase sidecar runner.
    #[allow(dead_code)]
    pub ext_dir: PathBuf,
    #[allow(dead_code)]
    pub kind: ExtKind,
    pub sidecar: Option<Sidecar>,
    pub command: ExtCommand,
    pub tools: HashMap<String, PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_extension_id() {
        assert!(validate_extension_id("my-extension_1").is_ok());
        assert!(validate_extension_id("valid-ext").is_ok());
        assert!(validate_extension_id("a").is_ok());

        assert!(validate_extension_id("").is_err());
        assert!(validate_extension_id("../traversal").is_err());
        assert!(validate_extension_id("nested/path").is_err());
        assert!(validate_extension_id("nested\\path").is_err());
        assert!(validate_extension_id("ext:name").is_err());
        assert!(validate_extension_id("ext;rm").is_err());
        assert!(validate_extension_id("UPPERCASE").is_err());
        assert!(validate_extension_id("ext with spaces").is_err());
        assert!(validate_extension_id("control\x00char").is_err());
    }
}
