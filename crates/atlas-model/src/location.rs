use crate::tag_cache::TagId;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Location {
    Directory(PathBuf),
    Archive {
        archive: PathBuf,
        inner: PathBuf,
    },
    Tag(TagId),
    RecycleBin,
    #[cfg(feature = "provider-host")]
    Remote {
        id: String,
        path: String,
    },
}

impl Location {
    /// Real filesystem directory for this location; `None` for archives, tags, remotes.
    /// Use for: filesystem watchers, DnD drops, paste destinations, goto-root.
    pub fn fs_dir(&self) -> Option<&Path> {
        match self {
            Self::Directory(p) => Some(p),
            _ => None,
        }
    }

    /// Path suitable for breadcrumbs and title bars.
    pub fn display_path(&self) -> PathBuf {
        match self {
            Self::Directory(p) => p.clone(),
            Self::Archive { archive, inner } => {
                if inner == Path::new("") {
                    archive.clone()
                } else {
                    archive.join(inner)
                }
            }
            Self::Tag(id) => PathBuf::from(format!("tag:{}", id.0)),
            Self::RecycleBin => PathBuf::from("Recycle Bin"),
            #[cfg(feature = "provider-host")]
            Self::Remote { id, path } => {
                PathBuf::from(format!("{id}:{}", crate::remote_path::display(path)))
            }
        }
    }

    /// `(archive_file, inner_path)` when browsing inside an archive, else `None`.
    pub fn as_archive(&self) -> Option<(&Path, &Path)> {
        match self {
            Self::Archive { archive, inner } => Some((archive, inner)),
            _ => None,
        }
    }

    pub fn is_directory(&self) -> bool {
        matches!(self, Self::Directory(_))
    }

    pub fn is_archive(&self) -> bool {
        matches!(self, Self::Archive { .. })
    }

    /// Navigate one level up, returning the parent `Location`.
    /// Returns `None` at filesystem roots and for tags/remotes.
    pub fn ascend(&self) -> Option<Location> {
        match self {
            Self::Directory(p) => {
                let parent = p.parent()?;
                if parent == p.as_path() {
                    return None;
                }
                Some(Self::Directory(parent.to_path_buf()))
            }
            Self::Archive { archive, inner } => {
                if inner == Path::new("") {
                    Some(Self::Directory(archive.parent()?.to_path_buf()))
                } else {
                    let parent_inner = inner.parent().unwrap_or(Path::new("")).to_path_buf();
                    Some(Self::Archive {
                        archive: archive.clone(),
                        inner: parent_inner,
                    })
                }
            }
            Self::Tag(_) | Self::RecycleBin => None,
            #[cfg(feature = "provider-host")]
            Self::Remote { id, path } => {
                let trimmed = path.trim_matches('/');
                if trimmed.is_empty() {
                    None
                } else {
                    let parent = match trimmed.rsplit_once('/') {
                        Some((p, _)) => p.to_string(),
                        None => String::new(),
                    };
                    Some(Self::Remote {
                        id: id.clone(),
                        path: parent,
                    })
                }
            }
        }
    }
}

impl Default for Location {
    fn default() -> Self {
        Self::Directory(PathBuf::new())
    }
}

#[cfg(all(test, feature = "provider-host"))]
mod tests {
    use super::*;

    #[test]
    fn test_remote_ascend() {
        let nested = Location::Remote {
            id: "drive".into(),
            path: "folder/subfolder".into(),
        };
        let parent = nested.ascend().unwrap();
        assert_eq!(
            parent,
            Location::Remote {
                id: "drive".into(),
                path: "folder".into(),
            }
        );

        let root_parent = parent.ascend().unwrap();
        assert_eq!(
            root_parent,
            Location::Remote {
                id: "drive".into(),
                path: String::new(),
            }
        );

        assert_eq!(root_parent.ascend(), None);
    }
}
