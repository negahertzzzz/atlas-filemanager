//! Polar.sh license keys. The customer-portal endpoints are public, so no
//! secret ships in the app; the key itself is the credential.
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

/// Polar organization id, injected at build time (`ATLAS_POLAR_ORG_ID`).
const ORG_ID: Option<&str> = option_env!("ATLAS_POLAR_ORG_ID");
const ENDPOINT: &str = "https://api.polar.sh/v1/customer-portal/license-keys";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The build has no organization id, so keys cannot be checked.
    NotConfigured,
    /// Unknown, revoked or disabled key.
    Invalid,
    /// The key is already active on its maximum number of devices.
    ActivationLimit,
    Network(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("License activation is not available in this build."),
            Self::Invalid => f.write_str("That license key is not valid."),
            Self::ActivationLimit => f.write_str(
                "This key is already active on its maximum number of devices. Deactivate one first.",
            ),
            Self::Network(why) => write!(f, "Could not reach the license server: {why}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Serialize)]
struct ActivateRequest<'a> {
    key: &'a str,
    organization_id: &'a str,
    label: &'a str,
}

#[derive(Deserialize)]
struct Activation {
    id: String,
}

#[derive(Serialize)]
struct KeyRequest<'a> {
    key: &'a str,
    organization_id: &'a str,
    activation_id: &'a str,
}

#[derive(Deserialize)]
struct Validation {
    status: String,
}

fn org() -> Result<&'static str, Error> {
    ORG_ID
        .filter(|id| !id.is_empty())
        .ok_or(Error::NotConfigured)
}

fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::Network(e.to_string()))
}

fn classify(status: reqwest::StatusCode) -> Error {
    match status.as_u16() {
        403 => Error::ActivationLimit,
        404 | 422 => Error::Invalid,
        code => Error::Network(format!("HTTP {code}")),
    }
}

async fn post<T: Serialize + Sync>(path: &str, body: &T) -> Result<reqwest::Response, Error> {
    let response = client()?
        .post(format!("{ENDPOINT}/{path}"))
        .json(body)
        .send()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(classify(response.status()))
    }
}

/// Binds `key` to this device and returns the activation id to store.
pub async fn activate(key: &str, label: &str) -> Result<String, Error> {
    let body = ActivateRequest {
        key,
        organization_id: org()?,
        label,
    };
    let activation: Activation = post("activate", &body)
        .await?
        .json()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    Ok(activation.id)
}

/// `Ok(true)` when the key is granted for this activation.
pub async fn validate(key: &str, activation_id: &str) -> Result<bool, Error> {
    let body = KeyRequest {
        key,
        organization_id: org()?,
        activation_id,
    };
    let validation: Validation = post("validate", &body)
        .await?
        .json()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    Ok(validation.status == "granted")
}

/// Frees this device's slot so the key can move to another PC.
pub async fn deactivate(key: &str, activation_id: &str) -> Result<(), Error> {
    let body = KeyRequest {
        key,
        organization_id: org()?,
        activation_id,
    };
    post("deactivate", &body).await.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn http_statuses_map_to_user_facing_errors() {
        assert_eq!(classify(StatusCode::FORBIDDEN), Error::ActivationLimit);
        assert_eq!(classify(StatusCode::NOT_FOUND), Error::Invalid);
        assert_eq!(classify(StatusCode::UNPROCESSABLE_ENTITY), Error::Invalid);
        assert_eq!(
            classify(StatusCode::BAD_GATEWAY),
            Error::Network("HTTP 502".into())
        );
    }

    #[test]
    fn responses_parse_with_unknown_fields_ignored() {
        let activation: Activation = serde_json::from_str(
            r#"{"id":"a1","license_key_id":"k1","label":"PC","license_key":{"status":"granted","limit_activations":3}}"#,
        )
        .unwrap();
        assert_eq!(activation.id, "a1");
        let validation: Validation =
            serde_json::from_str(r#"{"id":"k1","status":"revoked","usage":0}"#).unwrap();
        assert_ne!(validation.status, "granted");
    }
}
