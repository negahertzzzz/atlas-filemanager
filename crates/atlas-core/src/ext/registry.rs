#![allow(dead_code)]
// reason: retained API surface not exercised by every GPUI flow yet
// reason: extension-dir scan + manifest reads happen one-shot at startup/reload,
// off the gpui render thread; the sync-fs ban targets UI-foreground blocking.
#![allow(clippy::disallowed_methods)]
//! In-memory registry of extensions discovered under the extensions directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

static BINARY_PATH_CACHE: LazyLock<RwLock<HashMap<String, Option<PathBuf>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

use super::manifest::{ExtCommand, ExtKind, Manifest};
pub use atlas_model::ext_model::CommandRef;
use atlas_model::ext_model::ToolSpec;

/// A loaded extension and its runtime state.
#[derive(Debug, Clone)]
pub struct Extension {
    pub manifest: Manifest,
    /// Directory containing `extension.toml` (synthetic/empty for builtins).
    pub dir: PathBuf,
    pub enabled: bool,
    pub builtin: bool,
}

#[derive(Default)]
pub struct Registry {
    pub extensions: Vec<Extension>,
    tools: HashMap<String, PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ToolInstallRequest {
    pub provider_dir: PathBuf,
    pub tool: ToolSpec,
}

impl Registry {
    /// Build the registry: compiled-in builtins first, then every valid
    /// `extension.toml` under the extensions directory. `enabled` overrides the
    /// default-enabled state per extension id.
    pub fn load(enabled: &HashMap<String, bool>) -> Self {
        let mut extensions: Vec<Extension> = Vec::new();
        let dir = super::extensions_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let ext_dir = entry.path();
                if !ext_dir.is_dir() {
                    continue;
                }
                let manifest_path = ext_dir.join("extension.toml");
                let Ok(text) = std::fs::read_to_string(&manifest_path) else {
                    continue;
                };
                let manifest = match Manifest::parse(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("extension {ext_dir:?}: bad manifest: {e}");
                        continue;
                    }
                };
                // Builtins win on id collision.
                if extensions.iter().any(|e| e.manifest.id == manifest.id) {
                    continue;
                }
                let enabled = enabled.get(&manifest.id).copied().unwrap_or(true);
                extensions.push(Extension {
                    manifest,
                    dir: ext_dir,
                    enabled,
                    builtin: false,
                });
            }
        }
        let tools = collect_tools(&extensions);
        Self { extensions, tools }
    }

    pub fn get(&self, id: &str) -> Option<&Extension> {
        self.extensions.iter().find(|e| e.manifest.id == id)
    }

    // Only consumed by cloud-gated UI today; keep available without the feature.
    #[cfg_attr(not(feature = "provider-host"), allow(dead_code))]
    pub fn is_enabled(&self, id: &str) -> bool {
        self.get(id).is_some_and(|e| e.enabled)
    }

    pub fn has_sidebar_section(&self, id: &str) -> bool {
        self.sidebar_section(id).is_some()
    }

    pub fn sidebar_section(
        &self,
        id: &str,
    ) -> Option<&atlas_model::ext_model::SidebarContribution> {
        self.extensions
            .iter()
            .filter(|extension| extension.enabled)
            .flat_map(|extension| &extension.manifest.sidebar)
            .find(|section| section.id == id)
    }

    /// All enabled declarative commands as `(ext_id, command)`, unfiltered by
    /// selection. The context menu filters these by the right-clicked paths.
    pub fn declarative_commands(&self) -> Vec<(String, ExtCommand)> {
        self.extensions
            .iter()
            .filter(|e| e.enabled && !e.manifest.hidden && e.manifest.kind == ExtKind::Declarative)
            .flat_map(|e| {
                let id = e.manifest.id.clone();
                e.manifest
                    .commands
                    .iter()
                    .filter(|c| !c.thumbnail && !c.preview)
                    .map(move |c| (id.clone(), c.clone()))
            })
            .collect()
    }

    /// Enabled declarative commands that opted into the Actions ring, accept
    /// every path and have all their tools installed.
    pub fn ring_commands(&self, paths: &[std::path::PathBuf]) -> Vec<(String, ExtCommand)> {
        if paths.is_empty() {
            return Vec::new();
        }
        self.declarative_commands()
            .into_iter()
            .filter(|(_, command)| command.ring && self.required_tools(command).is_some())
            .filter(|(_, command)| {
                paths.iter().all(|path| {
                    let extension = path.extension().and_then(|e| e.to_str());
                    command.accepts.matches(extension, path.is_dir())
                })
            })
            .collect()
    }

    /// Resolve a specific command to a runnable `CommandRef`.
    pub fn command(&self, ext_id: &str, cmd_id: &str) -> Option<CommandRef> {
        let ext = self.get(ext_id)?;
        if !ext.enabled {
            return None;
        }
        let command = ext.manifest.commands.iter().find(|c| c.id == cmd_id)?;
        Some(Self::make_ref(ext, command, self.required_tools(command)?))
    }

    /// Find an enabled thumbnailer command that accepts files with extension `ext` and
    /// whose required binaries are on `PATH`. Returns the first match (builtins skipped).
    pub fn thumbnailer_for(&self, ext: &str) -> Option<CommandRef> {
        for extension in self.extensions.iter().filter(|e| e.enabled) {
            if extension.manifest.kind == ExtKind::Builtin {
                continue;
            }
            for cmd in &extension.manifest.commands {
                if !cmd.thumbnail {
                    continue;
                }
                if !cmd.accepts.matches(Some(ext), false) {
                    continue;
                }
                let Some(tools) = self.required_tools(cmd) else {
                    continue;
                };
                return Some(Self::make_ref(extension, cmd, tools));
            }
        }
        None
    }

    /// Find an enabled inspector previewer command that accepts files with extension
    /// `ext`. Returns the first match; required binaries must be present.
    pub fn previewer_for(&self, ext: &str) -> Option<CommandRef> {
        for extension in self.extensions.iter().filter(|e| e.enabled) {
            for cmd in &extension.manifest.commands {
                if !cmd.preview {
                    continue;
                }
                if !cmd.accepts.matches(Some(ext), false) {
                    continue;
                }
                let Some(tools) = self.required_tools(cmd) else {
                    continue;
                };
                return Some(Self::make_ref(extension, cmd, tools));
            }
        }
        None
    }

    pub fn missing_preview_tools_for(&self, ext: &str) -> Option<(String, Vec<String>)> {
        for extension in self.extensions.iter().filter(|e| e.enabled) {
            for cmd in &extension.manifest.commands {
                if !cmd.preview || !cmd.accepts.matches(Some(ext), false) {
                    continue;
                }
                let missing: Vec<String> = cmd
                    .requires
                    .iter()
                    .filter(|name| self.resolve_tool(name).is_none())
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    return Some((extension.manifest.name.clone(), missing));
                }
            }
        }
        None
    }

    pub fn missing_preview_tool_install_for(&self, ext: &str) -> Option<ToolInstallRequest> {
        let (_, missing) = self.missing_preview_tools_for(ext)?;
        missing
            .into_iter()
            .find_map(|name| self.install_request_for_tool(&name))
    }

    /// Commands offered for a selection, described as `(extension, is_dir)` pairs.
    /// Filters by enabled state, `accepts`, and required binaries being present.
    pub fn commands_for(&self, selection: &[(Option<String>, bool)]) -> Vec<CommandRef> {
        let mut out = Vec::new();
        for ext in self.extensions.iter().filter(|e| e.enabled) {
            if ext.manifest.hidden
                || matches!(ext.manifest.kind, ExtKind::Builtin | ExtKind::ToolProvider)
            {
                continue; // builtins wire their own UI
            }
            for cmd in &ext.manifest.commands {
                if cmd.thumbnail {
                    continue; // thumbnailers are background-only, not menu entries
                }
                if cmd.preview {
                    continue; // previewers are background-only, not menu entries
                }
                let applies = !selection.is_empty()
                    && selection
                        .iter()
                        .all(|(e, is_dir)| cmd.accepts.matches(e.as_deref(), *is_dir));
                if !applies {
                    continue;
                }
                let Some(tools) = self.required_tools(cmd) else {
                    continue;
                };
                out.push(Self::make_ref(ext, cmd, tools));
            }
        }
        out
    }

    /// File actions only; commands hidden here remain available in the palette.
    pub fn context_commands_for(&self, selection: &[(Option<String>, bool)]) -> Vec<CommandRef> {
        self.commands_for(selection)
            .into_iter()
            .filter(|cmd| cmd.command.context_menu)
            .collect()
    }

    pub fn resolve_tool(&self, name: &str) -> Option<PathBuf> {
        self.tools.get(name).cloned().or_else(|| binary_path(name))
    }

    /// In-memory paths only: safe to snapshot before background discovery.
    pub fn managed_tools(&self) -> HashMap<String, PathBuf> {
        self.tools.clone()
    }

    fn make_ref(ext: &Extension, cmd: &ExtCommand, tools: HashMap<String, PathBuf>) -> CommandRef {
        CommandRef {
            ext_id: ext.manifest.id.clone(),
            ext_dir: ext.dir.clone(),
            kind: ext.manifest.kind,
            sidecar: ext.manifest.sidecar.clone(),
            command: cmd.clone(),
            tools,
        }
    }

    fn required_tools(&self, cmd: &ExtCommand) -> Option<HashMap<String, PathBuf>> {
        let mut tools = HashMap::new();
        for name in &cmd.requires {
            tools.insert(name.clone(), self.resolve_tool(name)?);
        }
        Some(tools)
    }

    pub fn install_request_for_tool(&self, name: &str) -> Option<ToolInstallRequest> {
        for ext in self
            .extensions
            .iter()
            .filter(|e| e.enabled && e.manifest.kind == ExtKind::ToolProvider)
        {
            for tool in &ext.manifest.tools {
                if tool.name == name
                    && platform_matches(&tool.os, &tool.arch)
                    && !tool.download_url.is_empty()
                    && !tool.sha256.is_empty()
                    && tool_archive_supported(&tool.archive_format)
                {
                    return Some(ToolInstallRequest {
                        provider_dir: ext.dir.clone(),
                        tool: tool.clone(),
                    });
                }
            }
        }
        None
    }
}

fn tool_archive_supported(format: &str) -> bool {
    matches!(format, "zip" | "tgz") || cfg!(feature = "sevenz") && format == "7z"
}

fn collect_tools(extensions: &[Extension]) -> HashMap<String, PathBuf> {
    let mut tools = HashMap::new();
    for ext in extensions
        .iter()
        .filter(|e| e.enabled && e.manifest.kind == ExtKind::ToolProvider)
    {
        for tool in &ext.manifest.tools {
            if !platform_matches(&tool.os, &tool.arch) {
                continue;
            }
            // A provider's tools live inside its own directory; an absolute or
            // `..` path would let a manifest alias any program on the system.
            let Ok(relative) = super::tool_package::relative(&tool.path) else {
                tracing::warn!(ext = %ext.manifest.id, tool = %tool.name, "ignoring tool with unsafe path");
                continue;
            };
            let path = ext.dir.join(relative);
            if installed_tool_ok(&tool.name, &path) {
                tools.insert(tool.name.clone(), path);
            }
        }
    }
    tools
}

fn installed_tool_ok(name: &str, path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    match name {
        "pdftoppm" => path
            .parent()
            .is_some_and(|dir| dir.join("lcms2.dll").is_file()),
        _ => true,
    }
}

fn platform_matches(os: &str, arch: &str) -> bool {
    (os.is_empty() || os == std::env::consts::OS)
        && (arch.is_empty() || arch == std::env::consts::ARCH)
}

/// Is `name` an executable on `PATH`? Cheap PATH scan (no process spawn).
pub fn has_binary(name: &str) -> bool {
    binary_path(name).is_some()
}

pub fn binary_path(name: &str) -> Option<PathBuf> {
    if let Ok(cache) = BINARY_PATH_CACHE.read() {
        if let Some(res) = cache.get(name) {
            return res.clone();
        }
    }
    let res = binary_path_uncached(name);
    if let Ok(mut cache) = BINARY_PATH_CACHE.write() {
        cache.insert(name.to_string(), res.clone());
    }
    res
}

fn binary_path_uncached(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.is_file() {
        return Some(direct.to_path_buf());
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return None;
    };
    std::env::split_paths(&paths)
        .find_map(|dir| {
            let candidate = dir.join(name);
            if candidate.is_file() {
                Some(candidate)
            } else {
                let exe = candidate.with_extension("exe");
                exe.is_file().then_some(exe)
            }
        })
        .or_else(|| well_known_binary_path(name))
}

#[cfg(windows)]
fn well_known_binary_path(name: &str) -> Option<PathBuf> {
    match name {
        "magick" => find_in_program_files("ImageMagick", "magick.exe"),
        "gs" => find_in_program_files("gs", "gswin64c.exe"),
        _ => None,
    }
}

#[cfg(not(windows))]
fn well_known_binary_path(_name: &str) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn find_in_program_files(dir_prefix: &str, exe_name: &str) -> Option<PathBuf> {
    ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .flat_map(|root| std::fs::read_dir(root).ok().into_iter().flatten())
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find_map(|path| {
            let name = path.file_name()?.to_string_lossy();
            if name.starts_with(dir_prefix) {
                let exe = path.join(exe_name);
                exe.is_file().then_some(exe)
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with(manifest: Manifest) -> Registry {
        Registry {
            extensions: vec![Extension {
                manifest,
                dir: PathBuf::new(),
                enabled: true,
                builtin: false,
            }],
            tools: HashMap::new(),
        }
    }

    #[test]
    fn icon_commands_are_palette_only_and_file_actions_remain_in_context_menu() {
        let manifest = Manifest::parse(include_str!(
            "../../../../extensions/icon-manager/extension.toml"
        ))
        .unwrap();
        let registry = registry_with(manifest);
        let selection = [(Some("txt".into()), false)];
        assert_eq!(registry.commands_for(&selection).len(), 6);
        assert!(registry.context_commands_for(&selection).is_empty());
        assert!(registry
            .command("icon-manager", "icon-manager.download-material")
            .is_some());

        let manifest = Manifest::parse(
            r#"
id = "file-actions"
name = "File Actions"
[[commands]]
id = "convert"
title = "Convert"
accepts = { any = true }
"#,
        )
        .unwrap();
        assert_eq!(
            registry_with(manifest)
                .context_commands_for(&selection)
                .len(),
            1
        );
    }

    #[test]
    fn ring_commands_need_opt_in_matching_paths_and_installed_tools() {
        let manifest = Manifest::parse(
            r#"
id = "img"
name = "Img"
kind = "declarative"

[[commands]]
id = "ring-png"
title = "Ring PNG"
ring = true
accepts = { extensions = ["png"] }
run = "x {input}"

[[commands]]
id = "menu-only"
title = "Menu only"
accepts = { extensions = ["png"] }
run = "x {input}"

[[commands]]
id = "needs-tool"
title = "Needs tool"
ring = true
requires = ["definitely-not-installed-tool"]
accepts = { extensions = ["png"] }
run = "definitely-not-installed-tool {input}"
"#,
        )
        .unwrap();
        let registry = registry_with(manifest);
        let png = [std::path::PathBuf::from("a.png")];
        let ids: Vec<_> = registry.ring_commands(&png).into_iter().map(|(_, c)| c.id).collect();
        assert_eq!(ids, ["ring-png"]);
        let mixed = [std::path::PathBuf::from("a.png"), std::path::PathBuf::from("b.jpg")];
        assert!(registry.ring_commands(&mixed).is_empty());
        assert!(registry.ring_commands(&[]).is_empty());
    }

    #[test]
    fn preview_command_resolves_but_is_hidden_from_menus() {
        let manifest = Manifest::parse(
            r#"
id = "docs"
name = "Docs"
kind = "declarative"

[[commands]]
id = "csv-preview"
title = "CSV preview"
preview = true
output = "table"
accepts = { extensions = ["csv"] }
"#,
        )
        .unwrap();
        let registry = registry_with(manifest);

        assert!(registry.previewer_for("csv").is_some());
        assert!(registry
            .commands_for(&[(Some("csv".into()), false)])
            .is_empty());
        assert!(registry.declarative_commands().is_empty());
    }

    #[test]
    fn thumbnail_command_does_not_mask_preview_command() {
        let manifest = Manifest::parse(
            r#"
id = "docs"
name = "Docs"
kind = "declarative"

[[commands]]
id = "psd-thumb"
title = "PSD thumb"
thumbnail = true
accepts = { extensions = ["psd"] }

[[commands]]
id = "psd-preview"
title = "PSD preview"
preview = true
output = "image"
accepts = { extensions = ["psd"] }
"#,
        )
        .unwrap();
        let registry = registry_with(manifest);

        assert_eq!(
            registry.previewer_for("psd").unwrap().command.id,
            "psd-preview"
        );
    }

    #[test]
    fn tool_provider_satisfies_requires_without_menu_entry() {
        let root = std::env::temp_dir().join(format!("atlas-tools-{}", uuid::Uuid::new_v4()));
        let bin = root.join("tools").join("magick.exe");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"fake").unwrap();

        let provider = Manifest::parse(
            r#"
id = "runtime-tools"
name = "Runtime Tools"
kind = "tool_provider"
hidden = true

[[tools]]
name = "magick"
path = "tools/magick.exe"
"#,
        )
        .unwrap();
        let previews = Manifest::parse(
            r#"
id = "docs"
name = "Docs"
kind = "builtin"

[[commands]]
id = "psd-preview"
title = "PSD preview"
preview = true
requires = ["magick"]
accepts = { extensions = ["psd"] }
"#,
        )
        .unwrap();
        let extensions = vec![
            Extension {
                manifest: provider,
                dir: root.clone(),
                enabled: true,
                builtin: false,
            },
            Extension {
                manifest: previews,
                dir: PathBuf::new(),
                enabled: true,
                builtin: false,
            },
        ];
        let registry = Registry {
            tools: collect_tools(&extensions),
            extensions,
        };

        let cmd = registry.previewer_for("psd").unwrap();
        assert_eq!(cmd.tools.get("magick"), Some(&bin));
        assert_eq!(registry.managed_tools().get("magick"), Some(&bin));
        assert!(registry
            .commands_for(&[(Some("psd".into()), false)])
            .is_empty());

        let mut disabled = registry.extensions.clone();
        disabled[0].enabled = false;
        assert!(collect_tools(&disabled).is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unsupported_tool_archive_is_not_auto_installable() {
        let provider = Manifest::parse(
            r#"
id = "runtime-tools"
name = "Runtime Tools"
kind = "tool_provider"
hidden = true

[[tools]]
name = "magick"
path = "tools/magick.exe"
download_url = "https://example.test/magick.rar"
archive_format = "rar"
"#,
        )
        .unwrap();
        let previews = Manifest::parse(
            r#"
id = "docs"
name = "Docs"
kind = "builtin"

[[commands]]
id = "psd-preview"
title = "PSD preview"
preview = true
requires = ["magick"]
accepts = { extensions = ["psd"] }
"#,
        )
        .unwrap();
        let extensions = vec![
            Extension {
                manifest: provider,
                dir: PathBuf::new(),
                enabled: true,
                builtin: false,
            },
            Extension {
                manifest: previews,
                dir: PathBuf::new(),
                enabled: true,
                builtin: false,
            },
        ];
        let registry = Registry {
            tools: HashMap::new(),
            extensions,
        };

        assert!(registry.missing_preview_tool_install_for("psd").is_none());
    }
}
