//! Versioned JSON-lines protocol for out-of-process virtual file providers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub operation: Operation,
}

impl Request {
    pub const fn new(id: u64, operation: Operation) -> Self {
        Self {
            version: VERSION,
            id,
            operation,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
    PrepareShutdown,
    CancelRequest {
        request_id: u64,
    },
    Initialize {
        data_dir: String,
    },
    Configure {
        remote: RemoteConfig,
        secret: String,
    },
    RemoveRemote {
        remote_id: String,
    },
    List {
        remote_id: String,
        path: String,
    },
    Stat {
        remote_id: String,
        path: String,
    },
    Quota {
        remote_id: String,
    },
    InstallMountSupport {
        transfer_id: String,
    },
    CreateDirectory {
        remote_id: String,
        path: String,
    },
    Delete {
        remote_id: String,
        entries: Vec<EntryRef>,
    },
    Rename {
        remote_id: String,
        from: String,
        to: String,
    },
    Copy {
        remote_id: String,
        entries: Vec<EntryRef>,
        destination: String,
    },
    CopyObject {
        remote_id: String,
        from: String,
        to: String,
    },
    CopyBetween {
        transfer_id: String,
        source_id: String,
        source: String,
        destination_id: String,
        destination: String,
    },
    Download {
        transfer_id: String,
        remote_id: String,
        source: String,
        destination: String,
        #[serde(default)]
        byte_limit: Option<u64>,
    },
    Upload {
        transfer_id: String,
        remote_id: String,
        source: String,
        destination: String,
        #[serde(default)]
        byte_limit: Option<u64>,
    },
    Mount {
        remote_id: String,
        mount_point: String,
    },
    Unmount {
        remote_id: String,
        mount_point: String,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteConfig {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryRef {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Response {
    pub version: u32,
    pub id: u64,
    #[serde(flatten)]
    pub result: ResultPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResultPayload {
    Ok {
        #[serde(default)]
        value: Value,
    },
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified_unix_ms: Option<i64>,
    #[serde(default)]
    pub mime: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub version: u32,
    #[serde(flatten)]
    pub event: EventPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventPayload {
    Progress {
        transfer_id: String,
        done: u64,
        total: u64,
    },
    RemoteChanged {
        remote_id: String,
        path: String,
    },
    Status {
        remote_id: String,
        status: String,
        detail: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_as_versioned_json() {
        let request = Request::new(
            7,
            Operation::List {
                remote_id: "drive".into(),
                path: "docs".into(),
            },
        );
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        assert!(json.contains("\"version\":2"));
    }
}
