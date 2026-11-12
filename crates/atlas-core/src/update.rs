use anyhow::Result;
#[cfg(not(windows))]
use serde::Deserialize;
#[cfg(not(windows))]
use std::time::Duration;

pub const APP_VERSION: &str = match option_env!("ATLAS_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub const RELEASES_URL: &str = match option_env!("ATLAS_RELEASES_URL") {
    Some(url) => url,
    None => "https://github.com/neondeex/atlas-filemanager/releases",
};

pub const SOURCE_URL: &str = match option_env!("ATLAS_SOURCE_URL") {
    Some(url) => url,
    None => "https://atlasfm.modhyt.org/source-code",
};

#[cfg(not(windows))]
const RELEASE_API: &str = match option_env!("ATLAS_RELEASE_API") {
    Some(url) => url,
    None => "https://api.github.com/repos/neondeex/atlas-filemanager/releases/latest",
};

pub struct AppUpdate {
    pub version: String,
    pub url: String,
}

#[cfg(not(windows))]
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
}

#[cfg(any(not(windows), test))]
fn is_newer(current: &str, candidate: &str) -> Result<bool> {
    let current = semver::Version::parse(current.trim_start_matches('v'))?;
    let candidate = semver::Version::parse(candidate.trim_start_matches('v'))?;
    Ok(candidate.cmp_precedence(&current).is_gt())
}

#[cfg(windows)]
#[allow(clippy::unused_async)] // Keep the same API as the Linux update checker.
pub async fn check_for_update(_current_version: &str) -> Result<Option<AppUpdate>> {
    // Microsoft Store owns the Windows update channel. Contribution builds
    // must not advertise standalone EXE updates from the legacy R2 feed.
    Ok(None)
}

#[cfg(not(windows))]
pub async fn check_for_update(current_version: &str) -> Result<Option<AppUpdate>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let update = {
        let response = client.get(RELEASE_API).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let release: GitHubRelease = response.error_for_status()?.json().await?;
        if release.draft || release.prerelease {
            return Ok(None);
        }
        AppUpdate {
            version: release.tag_name.trim_start_matches('v').to_owned(),
            url: release.html_url,
        }
    };

    Ok(is_newer(current_version, &update.version)?.then_some(update))
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_updates_do_not_redirect_users_to_external_installers() {
        assert!(super::check_for_update("0.0.0").await.unwrap().is_none());
    }

    #[test]
    fn version_precedence_prevents_downgrade_notices() {
        assert!(is_newer("0.9.0", "v0.10.0").unwrap());
        assert!(!is_newer("0.10.0", "0.9.0").unwrap());
        assert!(!is_newer("0.10.0", "0.10.0").unwrap());
        assert!(!is_newer("1.0.0+local", "1.0.0+other").unwrap());
        assert!(is_newer("1.0.0-rc.1", "1.0.0").unwrap());
        assert!(!is_newer("1.0.0", "1.0.0-rc.1").unwrap());
        assert!(is_newer("invalid", "1.0.0").is_err());
    }
}
