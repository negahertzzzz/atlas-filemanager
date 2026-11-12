//! Sends a crash report to the Atlas API, only when the user asks for it.
//!
//! What leaves the machine: the panic text (message, location, thread,
//! version) and the symbolized stack. Minidumps stay local: they hold process
//! memory. The user's home path and account name are redacted first.

use std::{path::Path, time::Duration};

const ENDPOINT: &str = "https://atlasnative.com/api/crash-report";
/// Mirrors the Worker's per-field limit.
const TEXT_LIMIT: usize = 40 * 1024;

#[derive(serde::Serialize)]
struct Payload<'a> {
    version: &'a str,
    os: String,
    summary: &'a str,
    panic: String,
    stack: String,
}

/// Uploads the `.panic.txt` / `.stack.txt` among `reports`. Returns the
/// server's report id on success.
pub async fn send(
    reports: &[impl AsRef<Path>],
    version: &str,
    summary: Option<&str>,
) -> Result<String, String> {
    let read = |suffix: &str| {
        reports
            .iter()
            .map(AsRef::as_ref)
            .find(|p| p.to_string_lossy().ends_with(suffix))
            .and_then(|p| std::fs::read(p).ok())
            .map(|bytes| prepare(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_default()
    };
    let payload = Payload {
        version,
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        summary: &redact(summary.unwrap_or_default()),
        panic: read(".panic.txt"),
        stack: read(".stack.txt"),
    };
    if payload.panic.is_empty() && payload.stack.is_empty() {
        return Err("no readable report".into());
    }
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?
        .post(ENDPOINT)
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    #[derive(serde::Deserialize)]
    struct Ack {
        id: String,
    }
    Ok(response
        .json::<Ack>()
        .await
        .map(|ack| ack.id)
        .unwrap_or_default())
}

fn prepare(text: &str) -> String {
    let text = redact(text);
    if text.len() <= TEXT_LIMIT {
        return text;
    }
    let mut end = TEXT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Replaces the home directory and account name with placeholders.
fn redact(text: &str) -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    redact_with(text, &home, &user)
}

fn redact_with(text: &str, home: &str, user: &str) -> String {
    let mut out = text.to_owned();
    if home.len() > 3 {
        out = replace_ignore_case(&out, home, "%USERPROFILE%");
        out = replace_ignore_case(&out, &home.replace('\\', "/"), "%USERPROFILE%");
    }
    // Short names would mangle unrelated words.
    if user.chars().count() >= 3 {
        out = replace_ignore_case(&out, user, "<user>");
    }
    out
}

fn replace_ignore_case(text: &str, needle: &str, with: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_needle = needle.to_lowercase();
    // Lowercasing can change byte lengths outside ASCII; fall back to exact.
    if lower_text.len() != text.len() || lower_needle.len() != needle.len() {
        return text.replace(needle, with);
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, _) in lower_text.match_indices(&lower_needle) {
        out.push_str(&text[last..start]);
        out.push_str(with);
        last = start + needle.len();
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_and_account_are_redacted_case_insensitively() {
        let text = r"panicked at c:\users\Ana.Lopez\Documents\a.txt; thread of ana.lopez; C:/Users/Ana.Lopez/x";
        let out = redact_with(text, r"C:\Users\Ana.Lopez", "Ana.Lopez");
        assert!(!out.to_lowercase().contains("ana.lopez"), "{out}");
        assert!(out.contains(r"%USERPROFILE%\Documents\a.txt"), "{out}");
    }

    #[test]
    fn short_account_names_are_left_alone() {
        assert_eq!(redact_with("main thread", "", "ma"), "main thread");
    }

    #[test]
    fn long_reports_are_cut_on_a_char_boundary() {
        let text = "é".repeat(TEXT_LIMIT);
        let out = prepare(&text);
        assert!(out.len() <= TEXT_LIMIT && out.chars().all(|c| c == 'é'));
    }
}
