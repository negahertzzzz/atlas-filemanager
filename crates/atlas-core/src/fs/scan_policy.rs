//! Paths excluded from automatic recursive work. Explicit directory listings
//! remain available; background scans and watchers avoid Windows churn folders.
use std::path::Path;

pub fn is_scan_excluded(path: &Path) -> bool {
    if crate::vault::is_private_path(path) || crate::vault::live::is_private(path) {
        return true;
    }
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static ROOTS: OnceLock<Vec<String>> = OnceLock::new();
        let roots = ROOTS.get_or_init(|| {
            excluded_roots(
                &std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()),
                std::env::var("ProgramData").ok().as_deref(),
            )
        });
        excluded_by(path, roots)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(any(windows, test))]
fn normalized(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .trim_end_matches('\\')
        .to_lowercase()
}

#[cfg(any(windows, test))]
fn excluded_roots(windir: &str, program_data: Option<&str>) -> Vec<String> {
    let windows = windir.trim_end_matches(['/', '\\']);
    let mut roots: Vec<_> = [
        "ProgramData",
        "Temp",
        "Prefetch",
        r"System32\config",
        r"System32\LogFiles",
        r"System32\DriverStore",
        r"System32\winevt\Logs",
        r"System32\wbem\Logs",
        r"SoftwareDistribution\Download",
    ]
    .iter()
    .map(|suffix| normalized(Path::new(&format!("{windows}\\{suffix}"))))
    .collect();
    roots.extend(program_data.into_iter().map(|p| normalized(Path::new(p))));
    roots.retain(|p| !p.is_empty());
    roots
}

#[cfg(any(windows, test))]
fn excluded_by(path: &Path, roots: &[String]) -> bool {
    let path = normalized(path);
    roots.iter().any(|root| {
        path == *root
            || path
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('\\'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_policy_matches_whole_subtrees_without_hiding_unrelated_folders() {
        let roots = excluded_roots(r"D:\Windows", Some(r"D:\ProgramData"));
        for path in [
            r"d:\WINDOWS\System32\config\SOFTWARE",
            r"\\?\D:\Windows\Prefetch\a.pf",
            r"D:\ProgramData\vendor",
            r"D:\Windows\Temp\a",
            r"D:\Windows\SoftwareDistribution\Download",
        ] {
            assert!(excluded_by(Path::new(path), &roots), "{path}");
        }
        for path in [
            r"D:\Windows\System32\config-backup",
            r"D:\project\Temp",
            r"D:\Users\me\Temp\a",
            r"D:\ProgramDatabase",
            r"C:\Windows\Temp",
        ] {
            assert!(!excluded_by(Path::new(path), &roots), "{path}");
        }
    }
}
