#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::ext::registry::CommandRef;
use anyhow::{Context as _, Result};
use atlas_model::app_state::BgResult;

pub fn run_icon_manager(
    rt: &tokio::runtime::Handle,
    bg_tx: flume::Sender<BgResult>,
    cmd: CommandRef,
    answers: HashMap<String, String>,
) {
    let ext_id = cmd.ext_id.clone();
    let cmd_id = cmd.command.id.clone();
    let data_dir = cmd.ext_dir.clone();
    let tx = bg_tx.clone();

    rt.spawn(async move {
        let result = match cmd_id.as_str() {
            "icon-manager.download-theme" => {
                let theme_id = answers
                    .get("theme")
                    .map(String::as_str)
                    .unwrap_or("PKief/material-icon-theme")
                    .trim();
                download_and_activate_theme(&tx, &data_dir, theme_id).await
            }
            "icon-manager.download-material" => {
                download_and_activate_theme(&tx, &data_dir, "PKief/material-icon-theme").await
            }
            "icon-manager.download-catppuccin" => {
                download_and_activate_theme(&tx, &data_dir, "catppuccin/catppuccin-vsc-icons").await
            }
            "icon-manager.download-flow" => {
                download_and_activate_theme(&tx, &data_dir, "thang-nm/flow-icons").await
            }
            "icon-manager.select-theme" => select_theme_variant(&tx, &data_dir).await,
            "icon-manager.reset-default" => reset_default_icons(&data_dir),
            other => Err(anyhow::anyhow!("Unsupported icon-manager command: {other}")),
        };

        let error = result.err().map(|e| e.to_string());
        let _ = bg_tx.send(BgResult::ExtCommandDone {
            ext_id,
            error,
            dirs: Vec::new(),
        });
    });
}

pub async fn download_and_activate_theme(
    bg_tx: &flume::Sender<BgResult>,
    data_dir: &Path,
    extension_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        valid_extension_id(extension_id),
        "Invalid Open VSX extension id: {extension_id}"
    );

    let client = reqwest::Client::new();
    let metadata: serde_json::Value = client
        .get(format!("https://open-vsx.org/api/{extension_id}/latest"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let download_url = metadata
        .pointer("/files/download")
        .and_then(serde_json::Value::as_str)
        .context("Open VSX response has no download URL")?;

    let bytes = client
        .get(download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let slug = extension_id.replace('/', "_");
    let theme_root = data_dir.join(&slug);
    if theme_root.exists() {
        let _ = std::fs::remove_dir_all(&theme_root);
    }
    std::fs::create_dir_all(&theme_root)?;

    let bytes_clone = bytes.to_vec();
    let theme_root_clone = theme_root.clone();
    tokio::task::spawn_blocking(move || extract_zip(&bytes_clone, &theme_root_clone)).await??;

    let variants = read_package_variants(&theme_root)?;
    if variants.is_empty() {
        return Err(anyhow::anyhow!("Extension contains no icon themes"));
    }

    // The first variant activates right away; the others are listed in
    // Settings › File icons (see `installed_icon_variants`).
    let _ = bg_tx;
    let selected_rel_path = variants[0].theme_rel_path.clone();
    record_active_variant(data_dir, &slug, &selected_rel_path);
    build_mappings_from_theme_file(data_dir, &theme_root, &selected_rel_path)
}

#[derive(Clone, Debug)]
struct ThemeVariant {
    pkg_name: String,
    label: String,
    theme_root: PathBuf,
    theme_rel_path: String,
}

fn read_package_variants(theme_root: &Path) -> Result<Vec<ThemeVariant>> {
    let package_path = if theme_root.join("extension/package.json").is_file() {
        theme_root.join("extension/package.json")
    } else if theme_root.join("package.json").is_file() {
        theme_root.join("package.json")
    } else {
        anyhow::bail!("no package.json found in {:?}", theme_root);
    };
    let package: serde_json::Value = serde_json::from_reader(std::fs::File::open(package_path)?)?;

    let pkg_name = package
        .get("displayName")
        .or_else(|| package.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Icon Theme")
        .to_string();

    let themes = package
        .pointer("/contributes/iconThemes")
        .and_then(serde_json::Value::as_array)
        .context("Extension contains no contributes.iconThemes")?;

    let mut variants = Vec::new();
    for theme in themes {
        let id = theme.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let label = theme.get("label").and_then(|l| l.as_str()).unwrap_or(id);
        let display = if label.is_empty() { "Default" } else { label };
        if let Some(path) = theme.get("path").and_then(|p| p.as_str()) {
            variants.push(ThemeVariant {
                pkg_name: pkg_name.clone(),
                label: display.to_string(),
                theme_root: theme_root.to_path_buf(),
                theme_rel_path: path.trim_start_matches("./").to_string(),
            });
        }
    }

    Ok(variants)
}

pub async fn select_theme_variant(bg_tx: &flume::Sender<BgResult>, data_dir: &Path) -> Result<()> {
    let mut all_variants = Vec::new();
    let mut search_dirs = vec![data_dir.to_path_buf()];
    if let Some(home) = dirs::home_dir() {
        search_dirs.push(home.join(".vscode/extensions"));
        search_dirs.push(home.join(".vscode-insiders/extensions"));
    }

    for dir in search_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.join("extension/package.json").is_file()
                    || path.join("package.json").is_file()
                {
                    if let Ok(variants) = read_package_variants(&path) {
                        all_variants.extend(variants);
                    }
                }
            }
        }
    }

    if all_variants.is_empty() {
        return Err(anyhow::anyhow!(
            "No icon themes currently downloaded. Use an 'Install Icon Theme' command first."
        ));
    }

    let selected_variant = if all_variants.len() > 1 {
        let multiple_pkgs = {
            let first_pkg = &all_variants[0].pkg_name;
            all_variants.iter().any(|v| &v.pkg_name != first_pkg)
        };

        let items: Vec<String> = all_variants
            .iter()
            .map(|v| {
                if multiple_pkgs {
                    format!("{} ({})", v.label, v.pkg_name)
                } else {
                    v.label.clone()
                }
            })
            .collect();

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let _ = bg_tx.send(BgResult::ExtShowQuickPick {
            items: items.clone(),
            reply_tx,
        });

        let choice = tokio::task::spawn_blocking(move || reply_rx.recv())
            .await?
            .unwrap_or_default();

        if choice.is_empty() {
            return Ok(());
        }

        let found_idx = items.iter().position(|it| it == &choice);
        match found_idx {
            Some(idx) => all_variants[idx].clone(),
            None => return Ok(()),
        }
    } else {
        all_variants[0].clone()
    };

    build_mappings_from_theme_file(
        data_dir,
        &selected_variant.theme_root,
        &selected_variant.theme_rel_path,
    )?;
    if let Some(slug) = selected_variant.theme_root.file_name() {
        record_active_variant(
            data_dir,
            &slug.to_string_lossy(),
            &selected_variant.theme_rel_path,
        );
    }
    Ok(())
}

/// One icon-theme variant installed under the icon-manager data dir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledIconVariant {
    /// Install directory name (`publisher_name`).
    pub slug: String,
    pub pkg_name: String,
    pub label: String,
    pub theme_rel_path: String,
    /// How many variants the package ships (1 → show just the package name).
    pub siblings: usize,
}

const ACTIVE_VARIANT_FILE: &str = "active_variant.txt";

fn record_active_variant(data_dir: &Path, slug: &str, theme_rel_path: &str) {
    let _ = std::fs::write(
        data_dir.join(ACTIVE_VARIANT_FILE),
        format!("{slug}\n{theme_rel_path}"),
    );
}

/// `(slug, theme_rel_path)` of the variant last activated, if any.
pub fn active_icon_variant(data_dir: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(data_dir.join(ACTIVE_VARIANT_FILE)).ok()?;
    let (slug, rel) = text.trim().split_once('\n')?;
    Some((slug.to_string(), rel.to_string()))
}

/// Every variant of every theme downloaded into `data_dir`.
pub fn installed_icon_variants(data_dir: &Path) -> Vec<InstalledIconVariant> {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(slug) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let Ok(variants) = read_package_variants(&path) else {
            continue;
        };
        let siblings = variants.len();
        out.extend(variants.into_iter().map(|v| InstalledIconVariant {
            slug: slug.clone(),
            pkg_name: v.pkg_name,
            label: v.label,
            theme_rel_path: v.theme_rel_path,
            siblings,
        }));
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// Switch to an installed variant without downloading; completion goes
/// through the normal `ExtCommandDone` path (persist + live apply).
pub fn activate_icon_variant(
    rt: &tokio::runtime::Handle,
    bg_tx: flume::Sender<BgResult>,
    data_dir: PathBuf,
    slug: String,
    theme_rel_path: String,
) {
    rt.spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            anyhow::ensure!(
                !slug.contains(['/', '\\']) && !slug.contains(".."),
                "invalid theme dir"
            );
            build_mappings_from_theme_file(&data_dir, &data_dir.join(&slug), &theme_rel_path)?;
            record_active_variant(&data_dir, &slug, &theme_rel_path);
            Ok(())
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        let _ = bg_tx.send(BgResult::ExtCommandDone {
            ext_id: "icon-manager".to_string(),
            error: result.err().map(|e| e.to_string()),
            dirs: Vec::new(),
        });
    });
}

pub fn reset_default_icons(data_dir: &Path) -> Result<()> {
    let _ = std::fs::remove_file(data_dir.join(ACTIVE_VARIANT_FILE));
    let mappings_path = data_dir.join("theme_mappings.json");
    if mappings_path.exists() {
        let _ = std::fs::remove_file(mappings_path);
    }
    Ok(())
}

fn valid_extension_id(value: &str) -> bool {
    let mut parts = value.split('/');
    let valid = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.starts_with('.')
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    parts.next().is_some_and(valid) && parts.next().is_some_and(valid) && parts.next().is_none()
}

fn extract_zip(bytes: &[u8], destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let output = destination.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(output)?;
        } else {
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(output)?;
            std::io::copy(&mut entry, &mut file)?;
        }
    }
    Ok(())
}

fn build_mappings_from_theme_file(
    data_dir: &Path,
    theme_root: &Path,
    theme_rel_path: &str,
) -> Result<()> {
    let base = if theme_root.join("extension").is_dir() {
        theme_root.join("extension")
    } else {
        theme_root.to_path_buf()
    };
    let theme_file = base.join(theme_rel_path);
    let contents = std::fs::read_to_string(&theme_file)
        .context(format!("Failed to read theme file at {:?}", theme_file))?;
    let clean = contents
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let theme: serde_json::Value = serde_json::from_str(&clean)?;

    let definitions = theme
        .get("iconDefinitions")
        .and_then(serde_json::Value::as_object)
        .context("Theme contains no icon definitions")?;
    let theme_dir = theme_file
        .parent()
        .context("Theme has no parent directory")?;

    let mut mappings = serde_json::Map::new();

    // Map default folder
    if let Some(folder_id) = theme.get("folder").and_then(serde_json::Value::as_str) {
        if let Some(icon_path) = definitions
            .get(folder_id)
            .and_then(|d| d.get("iconPath"))
            .and_then(serde_json::Value::as_str)
        {
            if let Ok(path) = normalize_icon_path(&base, theme_dir, icon_path) {
                mappings.insert(
                    "/".to_string(),
                    serde_json::Value::String(path.to_string_lossy().into_owned()),
                );
            }
        }
    }

    // Map generic file
    if let Some(file_id) = theme.get("file").and_then(serde_json::Value::as_str) {
        if let Some(icon_path) = definitions
            .get(file_id)
            .and_then(|d| d.get("iconPath"))
            .and_then(serde_json::Value::as_str)
        {
            if let Ok(path) = normalize_icon_path(&base, theme_dir, icon_path) {
                mappings.insert(
                    "".to_string(),
                    serde_json::Value::String(path.to_string_lossy().into_owned()),
                );
            }
        }
    }

    // Map file extensions
    if let Some(extensions) = theme
        .get("fileExtensions")
        .and_then(serde_json::Value::as_object)
    {
        for (extension, icon_id) in extensions {
            let Some(icon_path) = icon_id
                .as_str()
                .and_then(|id| definitions.get(id))
                .and_then(|definition| definition.get("iconPath"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            if let Ok(path) = normalize_icon_path(&base, theme_dir, icon_path) {
                mappings.insert(
                    extension.clone(),
                    serde_json::Value::String(path.to_string_lossy().into_owned()),
                );
            }
        }
    }

    let mut output = std::fs::File::create(data_dir.join("theme_mappings.json"))?;
    serde_json::to_writer(&mut output, &mappings)?;
    output.flush()?;
    Ok(())
}

fn normalize_icon_path(root: &Path, base: &Path, relative: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = base
        .join(relative.trim_start_matches("./"))
        .canonicalize()?;
    anyhow::ensure!(
        path.starts_with(root),
        "Icon path escapes extension directory"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_extension_ids() {
        assert!(valid_extension_id("PKief/material-icon-theme"));
        assert!(valid_extension_id("thang-nm/flow-icons"));
        assert!(valid_extension_id("catppuccin/catppuccin-vsc-icons"));
        assert!(!valid_extension_id("invalid"));
        assert!(!valid_extension_id("../traversal"));
        assert!(!valid_extension_id("a/b/c"));
    }

    #[test]
    fn parses_multiple_variants_from_package_json() {
        let temp = std::env::temp_dir().join(format!(
            "pkg_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ext_dir = temp.join("extension");
        std::fs::create_dir_all(&ext_dir).unwrap();

        let pkg_json = r#"{
            "name": "flow-icons",
            "displayName": "Flow Icons",
            "contributes": {
                "iconThemes": [
                    { "id": "flow-dim", "label": "Flow Dim", "path": "./dim.json" },
                    { "id": "flow-deep", "label": "Flow Deep", "path": "./deep.json" },
                    { "id": "flow-dawn", "label": "Flow Dawn", "path": "./dawn.json" },
                    { "id": "flow-you", "label": "Flow You", "path": "./you.json" }
                ]
            }
        }"#;
        std::fs::write(ext_dir.join("package.json"), pkg_json).unwrap();

        let variants = read_package_variants(&temp).unwrap();
        std::fs::remove_dir_all(&temp).ok();

        assert_eq!(variants.len(), 4);
        assert_eq!(variants[0].label, "Flow Dim");
        assert_eq!(variants[0].theme_rel_path, "dim.json");
        assert_eq!(variants[1].label, "Flow Deep");
        assert_eq!(variants[2].label, "Flow Dawn");
        assert_eq!(variants[3].label, "Flow You");
    }
}
