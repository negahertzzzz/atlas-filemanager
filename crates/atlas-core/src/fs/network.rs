//! Native SMB locations: `\\server`, `\\server\share\…` and drive letters
//! mapped to shares. Path parsing is platform-neutral; the Win32 work lives in
//! `atlas_win_space::network`.
use atlas_model::file_entry::{FileEntry, FileEntryData, FileKind};
use std::path::{Path, PathBuf};

/// Error text for a listing that failed because the server wants (other)
/// credentials. The UI answers it with the Windows credential prompt.
pub const CREDENTIALS_REQUIRED: &str = "network_credentials_required";

fn strip_ci<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

/// `(server, share)` of an SMB UNC path. Accepts `\\server\share\…`,
/// `//server/share` and the verbatim `\\?\UNC\server\share` form. Device
/// paths and WSL (`\\wsl$`, `\\wsl.localhost`) are not SMB and yield `None`.
pub fn unc_parts(path: &Path) -> Option<(String, Option<String>)> {
    let text = path.to_str()?;
    let rest = if let Some(rest) = strip_ci(text, r"\\?\UNC\") {
        rest
    } else if text.starts_with(r"\\?\") || text.starts_with(r"\\.\") {
        return None;
    } else {
        text.strip_prefix(r"\\")
            .or_else(|| text.strip_prefix("//"))?
    };
    let mut parts = rest.split(['\\', '/']).filter(|part| !part.is_empty());
    let server = parts.next()?;
    if server.eq_ignore_ascii_case("wsl$")
        || server.eq_ignore_ascii_case("wsl.localhost")
        || matches!(server, "." | "?")
    {
        return None;
    }
    Some((server.to_owned(), parts.next().map(str::to_owned)))
}

/// The server of a bare `\\server` path (no share), whose listing is the
/// server's share list rather than a directory.
pub fn bare_server(path: &Path) -> Option<String> {
    match unc_parts(path)? {
        (server, None) => Some(server),
        _ => None,
    }
}

/// `\\server\share` for any path inside an SMB share.
pub fn share_root(path: &Path) -> Option<PathBuf> {
    match unc_parts(path)? {
        (server, Some(share)) => Some(PathBuf::from(format!(r"\\{server}\{share}"))),
        _ => None,
    }
}

/// True for `\\server\share` itself (with or without a trailing separator).
pub fn is_share_root(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    let trimmed = text.trim_end_matches(['\\', '/']);
    share_root(path).is_some_and(|root| root.as_os_str().eq_ignore_ascii_case(trimmed))
}

/// Parent used by "Up": a share's parent is its server, like in Explorer.
pub fn parent(path: &Path) -> Option<PathBuf> {
    if is_share_root(path) {
        let (server, _) = unc_parts(path)?;
        return Some(PathBuf::from(format!(r"\\{server}")));
    }
    None
}

/// Drive letter of `path` when it is a drive-rooted path (`Z:\…`).
fn drive_letter(path: &Path) -> Option<String> {
    let bytes = path.to_str()?.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| format!("{}:", char::from(bytes[0].to_ascii_uppercase())))
}

/// True when `path` lives on SMB: a UNC share or a mapped network drive.
pub fn is_network_path(path: &Path) -> bool {
    if unc_parts(path).is_some() {
        return true;
    }
    #[cfg(windows)]
    {
        drive_letter(path).is_some_and(|letter| atlas_win_space::network::is_remote_drive(&letter))
    }
    #[cfg(not(windows))]
    {
        let _ = drive_letter(path);
        false
    }
}

/// The server a sign-in for `path` goes to: the UNC server, or the server
/// behind a mapped or remembered drive letter. Local disks answer `None` from
/// their drive type alone, without touching the network.
pub fn server_of(path: &Path) -> Option<String> {
    if let Some((server, _)) = unc_parts(path) {
        return Some(server);
    }
    #[cfg(windows)]
    {
        let letter = drive_letter(path)?;
        let remote = atlas_win_space::network::network_remote_of(&letter)?;
        unc_parts(Path::new(&remote)).map(|(server, _)| server)
    }
    #[cfg(not(windows))]
    {
        let _ = drive_letter(path);
        None
    }
}

/// The connection to (re)authenticate for `path`: its `\\server\share`, the
/// share behind a mapped drive, or the server's `IPC$` for a share list.
pub fn connection_target(path: &Path) -> Option<String> {
    if let Some(server) = bare_server(path) {
        return Some(format!(r"\\{server}\IPC$"));
    }
    if let Some(root) = share_root(path) {
        return root.to_str().map(str::to_owned);
    }
    #[cfg(windows)]
    {
        drive_letter(path).and_then(|letter| atlas_win_space::network::remote_of(&letter))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// A remembered mapped drive that `path` is on but that is not connected in
/// this session: `(drive letter, \\server\share)`. Opening it reconnects the
/// share under the same letter, as File Explorer does.
pub fn reconnect_target(path: &Path) -> Option<(String, String)> {
    #[cfg(windows)]
    {
        let letter = drive_letter(path)?;
        atlas_win_space::network::reconnectable(&letter).map(|remote| (letter, remote))
    }
    #[cfg(not(windows))]
    {
        let _ = drive_letter(path);
        None
    }
}

/// Whether a failed listing of `path` should offer the credential prompt
/// (or, for a remembered mapped drive, reconnect it).
pub fn wants_credentials(path: &Path, error: &anyhow::Error) -> bool {
    let code = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .and_then(std::io::Error::raw_os_error);
    #[cfg(windows)]
    {
        reconnect_target(path).is_some()
            || code.is_some_and(atlas_win_space::network::credentials_may_help)
                && is_network_path(path)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, code);
        false
    }
}

/// The shares of `\\server` as folder entries, so a server opens like a
/// directory. Administrative shares (`C$`, `ADMIN$`) follow `show_hidden`.
#[cfg(windows)]
pub fn share_entries(server: &str, show_hidden: bool) -> std::io::Result<Vec<FileEntry>> {
    let base = PathBuf::from(format!(r"\\{server}"));
    let directory = atlas_model::file_entry::EntryDirectory::new(base.clone());
    let shares = atlas_win_space::network::shares(server)?;
    Ok(shares
        .into_iter()
        .filter(|share| show_hidden || !share.special)
        .map(|share| {
            FileEntry::in_directory(
                FileEntryData {
                    path: PathBuf::from(format!(r"\\{server}\{}", share.name)),
                    name: share.name.into(),
                    kind: FileKind::Dir,
                    size: None,
                    modified: None,
                    _is_hidden: share.special,
                    _is_symlink: false,
                    archive_root: None,
                    recycle_info: None,
                },
                directory.clone(),
            )
        })
        .collect())
}

#[cfg(not(windows))]
pub fn share_entries(_server: &str, _show_hidden: bool) -> std::io::Result<Vec<FileEntry>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "SMB share lists are only available on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_forms_are_parsed_and_wsl_is_not_smb() {
        assert_eq!(
            unc_parts(Path::new(r"\\nas\media\film")),
            Some(("nas".into(), Some("media".into())))
        );
        assert_eq!(
            unc_parts(Path::new(r"\\?\UNC\nas\media\film")),
            Some(("nas".into(), Some("media".into())))
        );
        assert_eq!(
            unc_parts(Path::new("//nas/media")),
            Some(("nas".into(), Some("media".into())))
        );
        assert_eq!(unc_parts(Path::new(r"\\nas")), Some(("nas".into(), None)));
        assert_eq!(unc_parts(Path::new(r"\\nas\")), Some(("nas".into(), None)));
        for other in [
            r"\\wsl$\Ubuntu",
            r"\\wsl.localhost\Arch\home",
            r"\\?\C:\x",
            r"\\.\pipe\x",
            r"C:\x",
            "/home",
        ] {
            assert_eq!(unc_parts(Path::new(other)), None, "{other}");
        }
    }

    #[test]
    fn share_roots_parents_and_targets() {
        assert_eq!(
            share_root(Path::new(r"\\nas\media\a\b")),
            Some(PathBuf::from(r"\\nas\media"))
        );
        assert!(is_share_root(Path::new(r"\\nas\media")));
        assert!(is_share_root(Path::new(r"\\nas\media\")));
        assert!(!is_share_root(Path::new(r"\\nas\media\a")));
        assert_eq!(
            parent(Path::new(r"\\nas\media\")),
            Some(PathBuf::from(r"\\nas"))
        );
        assert_eq!(parent(Path::new(r"\\nas\media\a")), None);
        assert_eq!(bare_server(Path::new(r"\\nas")), Some("nas".into()));
        assert_eq!(bare_server(Path::new(r"\\nas\media")), None);
        assert_eq!(
            connection_target(Path::new(r"\\nas")),
            Some(r"\\nas\IPC$".into())
        );
        assert_eq!(
            connection_target(Path::new(r"\\nas\media\x")),
            Some(r"\\nas\media".into())
        );
    }

    #[test]
    fn only_network_access_errors_ask_for_credentials() {
        let denied = anyhow::Error::from(std::io::Error::from_raw_os_error(5));
        let missing = anyhow::Error::from(std::io::Error::from_raw_os_error(3));
        assert_eq!(
            wants_credentials(Path::new(r"\\nas\media"), &denied),
            cfg!(windows)
        );
        assert!(!wants_credentials(Path::new(r"\\nas\media"), &missing));
        assert!(!wants_credentials(
            Path::new(r"C:\Windows\System32\config"),
            &denied
        ));
    }
}
