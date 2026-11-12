//! At most two failed sign-ins per server, then a pause.
//!
//! Every refused SMB or cloud sign-in is a failed logon on the server. Domain
//! controllers lock accounts that keep failing, and security monitors flag
//! clients that retry. Atlas therefore stops contacting a server after
//! [`MAX_FAILED_SIGN_INS`] refused sign-ins and stays away for [`PAUSE`], also
//! across restarts. A successful sign-in clears the count; a failure older
//! than the pause no longer counts.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Refused sign-ins in a row after which Atlas stops trying.
pub const MAX_FAILED_SIGN_INS: u32 = 2;
/// How long a server stays paused. Matches the usual Active Directory
/// "reset account lockout counter after" window.
pub const PAUSE: Duration = Duration::from_secs(30 * 60);
/// Prefix of the error text for an operation refused because its server is
/// paused; [`paused_error`] appends the minutes left.
pub const PAUSED: &str = "network_signin_paused";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    failures: u32,
    /// Unix seconds of the latest refused sign-in.
    last_failure: u64,
}

impl Record {
    fn expired(self, now: u64) -> bool {
        now >= self.last_failure.saturating_add(PAUSE.as_secs())
    }

    fn remaining(self, now: u64) -> Option<Duration> {
        (self.failures >= MAX_FAILED_SIGN_INS && !self.expired(now))
            .then(|| Duration::from_secs(self.last_failure + PAUSE.as_secs() - now))
    }

    fn fail(self, now: u64) -> Self {
        let failures = if self.expired(now) {
            1
        } else {
            self.failures.saturating_add(1)
        };
        Self {
            failures,
            last_failure: now,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn key(server: &str) -> String {
    server.trim_matches(['\\', '/']).to_lowercase()
}

fn file() -> PathBuf {
    crate::util::config::config_dir().join("network-guard.json")
}

fn records() -> &'static Mutex<HashMap<String, Record>> {
    static RECORDS: OnceLock<Mutex<HashMap<String, Record>>> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let now = now();
        let mut loaded: HashMap<String, Record> = std::fs::read(file())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        loaded.retain(|_, record| !record.expired(now));
        Mutex::new(loaded)
    })
}

fn save(records: &HashMap<String, Record>) {
    let Ok(bytes) = serde_json::to_vec(records) else {
        return;
    };
    let path = file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = crate::util::config::write_atomic(&path, &bytes) {
        tracing::warn!(%error, "cannot save network sign-in pauses");
    }
}

/// Time left before Atlas contacts `server` again, or `None` when it may.
/// `server` is a host name or a `cloud:<remote>` key.
pub fn paused_for(server: &str) -> Option<Duration> {
    let records = records().lock().unwrap_or_else(PoisonError::into_inner);
    records
        .get(&key(server))
        .and_then(|record| record.remaining(now()))
}

/// Counts one refused sign-in to `server`. Returns the pause that starts now,
/// if this failure reached the limit.
pub fn record_failure(server: &str) -> Option<Duration> {
    let now = now();
    let mut records = records().lock().unwrap_or_else(PoisonError::into_inner);
    let record = records.entry(key(server)).or_default();
    *record = record.fail(now);
    let paused = record.remaining(now);
    tracing::warn!(
        server,
        failures = record.failures,
        paused = paused.is_some(),
        "network sign-in refused"
    );
    save(&records);
    paused
}

/// A sign-in to `server` succeeded: forget its failures.
pub fn record_success(server: &str) {
    let mut records = records().lock().unwrap_or_else(PoisonError::into_inner);
    if records.remove(&key(server)).is_some() {
        save(&records);
    }
}

/// Error text for an operation refused while `server` is paused.
pub fn paused_error(remaining: Duration) -> String {
    format!("{PAUSED}:{}", remaining.as_secs().div_ceil(60).max(1))
}

/// Minutes left, when `text` came from [`paused_error`].
pub fn parse_paused(text: &str) -> Option<u64> {
    text.strip_prefix(PAUSED)?.strip_prefix(':')?.parse().ok()
}

/// Windows error codes that mean the server refused the sign-in itself (as
/// opposed to an unreachable server or a folder the account may not open).
pub const fn is_sign_in_failure(code: i32) -> bool {
    matches!(
        code,
        86      // ERROR_INVALID_PASSWORD
        | 1244  // ERROR_NOT_AUTHENTICATED
        | 1272  // guest sign-in blocked by policy
        | 1326  // ERROR_LOGON_FAILURE
        | 1327  // ERROR_ACCOUNT_RESTRICTION
        | 1328  // ERROR_INVALID_LOGON_HOURS
        | 1329  // ERROR_INVALID_WORKSTATION
        | 1330  // ERROR_PASSWORD_EXPIRED
        | 1331  // ERROR_ACCOUNT_DISABLED
        | 1385  // ERROR_LOGON_TYPE_NOT_GRANTED
        | 1907  // ERROR_PASSWORD_MUST_CHANGE
        | 1909  // ERROR_ACCOUNT_LOCKED_OUT
        | 2202 // NERR_BadUsername
    )
}

/// The Windows error code inside `error`, if any.
pub fn os_code(error: &anyhow::Error) -> Option<i32> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .and_then(std::io::Error::raw_os_error)
}

/// Whether an rclone/cloud error message says the server refused the
/// credentials (SMB, WebDAV, FTP, SFTP and HTTP wording).
pub fn is_sign_in_failure_text(text: &str) -> bool {
    const PATTERNS: [&str; 12] = [
        "attempted logon is invalid",
        "logon failure",
        "status_logon_failure",
        "account is locked",
        "account has been locked",
        "status_account_locked_out",
        "password has expired",
        "unable to authenticate",
        "authentication failed",
        "login incorrect",
        "401 unauthorized",
        "invalid username or password",
    ];
    let text = text.to_lowercase();
    PATTERNS.iter().any(|pattern| text.contains(pattern))
}

/// Guard key for a cloud remote.
pub fn cloud_key(remote: &str) -> String {
    format!("cloud:{remote}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_failures_pause_and_the_pause_expires() {
        let start = 1_000_000;
        let one = Record::default().fail(start);
        assert_eq!(one.remaining(start), None, "one failure never pauses");
        let two = one.fail(start + 5);
        let left = two.remaining(start + 5).unwrap();
        assert_eq!(left, PAUSE);
        assert_eq!(two.remaining(start + 5 + PAUSE.as_secs()), None);
        // A failure after the pause starts a new count.
        assert_eq!(two.fail(start + 5 + PAUSE.as_secs()).failures, 1);
    }

    #[test]
    fn stale_failures_do_not_add_up() {
        let old = Record::default().fail(0);
        assert_eq!(old.fail(PAUSE.as_secs()).failures, 1);
    }

    #[test]
    fn paused_error_round_trips_minutes() {
        assert_eq!(
            parse_paused(&paused_error(Duration::from_secs(61))),
            Some(2)
        );
        assert_eq!(parse_paused(&paused_error(Duration::from_secs(1))), Some(1));
        assert_eq!(parse_paused("network_credentials_required"), None);
    }

    #[test]
    fn only_refused_sign_ins_count() {
        for code in [86, 1326, 1909, 2202] {
            assert!(is_sign_in_failure(code));
        }
        // Access denied to a folder, unreachable server, missing share.
        for code in [5, 53, 67, 1219, 1231] {
            assert!(!is_sign_in_failure(code));
        }
        assert!(is_sign_in_failure_text(
            "smb: response error: The attempted logon is invalid. This is either due to a bad username or authentication information."
        ));
        assert!(!is_sign_in_failure_text("directory not found"));
    }

    #[test]
    fn keys_ignore_case_and_slashes() {
        assert_eq!(key(r"\\SrvDati"), "srvdati");
        assert_eq!(cloud_key("srvdati"), "cloud:srvdati");
    }
}
