//! Drive object addressing. Mutations use immutable IDs, never a guessed path
//! amongst duplicate names. Tokens stay inside the provider process.
use super::rc_api::{AsyncJob, RcClient};
use anyhow::Context as _;
use atlas_model::remote_path;
use serde_json::{json, Value};

impl RcClient {
    pub(crate) async fn remote_config(&self, id: &str) -> anyhow::Result<Value> {
        self.call("config/get", json!({ "name": id })).await
    }
    pub(crate) async fn is_drive(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self
            .remote_config(id)
            .await?
            .get("type")
            .and_then(Value::as_str)
            == Some("drive"))
    }
    pub(crate) async fn scope(&self, id: &str, path: &str) -> anyhow::Result<(String, String)> {
        let parts: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
        for (index, part) in parts.iter().enumerate().rev() {
            if let Some(object) = remote_path::component(part) {
                anyhow::ensure!(
                    self.is_drive(id).await?,
                    "Object addresses require a Google Drive account"
                );
                if object.is_dir {
                    anyhow::ensure!(parts.len() <= index + 2,
                        "Refresh the destination folder to obtain its immutable object ID");
                    return Ok((
                        format!("{id},root_folder_id={}:", object.id),
                        parts[index + 1..].join("/"),
                    ));
                }
                anyhow::bail!("A file object cannot be used as a remote directory or destination");
            }
        }
        anyhow::ensure!(!path.contains('/') || !self.is_drive(id).await?,
            "Refresh the destination folder to obtain its immutable object ID");
        Ok((format!("{id}:"), path.to_string()))
    }
    pub(crate) async fn copy_id_async(
        &self,
        account: &str,
        object_id: &str,
        destination: &str,
        group: &str,
        byte_limit: Option<u64>,
    ) -> anyhow::Result<AsyncJob> {
        let mut body = json!({
            "fs": format!("{account}:"), "command": "copyid", "arg": [object_id, destination],
            "_async": true, "_group": group, "_config": {"Immutable":true, "IgnoreExisting":true},
        });
        if let Some(limit) = byte_limit {
            body["_config"]["MaxTransfer"] = json!(limit.to_string());
        }
        let result = self.call("backend/command", body).await?;
        Ok(AsyncJob {
            jobid: result
                .get("jobid")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::Error::new(crate::cloud::control::OutcomeUnknown("copyid accepted without a job ID; verify the destination before retrying".into())))?,
            group: group.into(),
        })
    }
    async fn drive_request(
        &self,
        account: &str,
        object_id: &str,
        body: Option<Value>,
        query: &[(&str, String)],
    ) -> anyhow::Result<Value> {
        // Trigger rclone's OAuth refresh before retrieving its current token.
        self.about(&format!("{account}:")).await?;
        let config = self.remote_config(account).await?;
        let token: Value = serde_json::from_str(
            config
                .get("token")
                .and_then(Value::as_str)
                .context("Drive OAuth token is unavailable")?,
        )?;
        let access = token
            .get("access_token")
            .and_then(Value::as_str)
            .context("Drive access token is unavailable")?;
        let mut url = reqwest::Url::parse(&self.drive_api)?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid Drive API base URL"))?
            .pop_if_empty()
            .push(object_id);
        // Reuse the pooled client: repeated Drive calls keep their TLS
        // connection instead of a new handshake each. Credentials are set
        // per request, so nothing of the local rc login reaches Google.
        let client = &self.http;
        let request = if let Some(body) = body {
            client.patch(url).json(&body)
        } else {
            client.get(url)
        };
        let response = request
            .bearer_auth(access)
            .query(&[("supportsAllDrives", "true")])
            .query(query)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }
    pub(crate) async fn trash_id(&self, account: &str, path: &str) -> anyhow::Result<()> {
        let object = remote_path::last(path)
            .context("Refresh the folder: deleting Drive objects requires their immutable ID")?;
        self.drive_request(account, &object.id, Some(json!({"trashed":true})), &[])
            .await?;
        Ok(())
    }
    pub(crate) async fn rename_id(
        &self,
        account: &str,
        from: &str,
        to: &str,
    ) -> anyhow::Result<()> {
        let object = remote_path::last(from)
            .context("Refresh the folder: moving Drive objects requires their immutable ID")?;
        let source_parent = from.rsplit_once('/').map_or("", |(parent, _)| parent);
        let target_parent = to.rsplit_once('/').map_or("", |(parent, _)| parent);
        let name = remote_path::file_name(to);
        let mut query = Vec::new();
        if source_parent != target_parent {
            let parent_id = if let Some(parent) = remote_path::last(target_parent) {
                anyhow::ensure!(parent.is_dir, "Destination is not a folder");
                parent.id
            } else {
                anyhow::ensure!(
                    target_parent.is_empty(),
                    "Refresh the destination folder to obtain its object ID"
                );
                let config = self.remote_config(account).await?;
                match config
                    .get("root_folder_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    Some(id) => id.to_string(),
                    None => self
                        .drive_request(account, "root", None, &[("fields", "id".into())])
                        .await?
                        .get("id")
                        .and_then(Value::as_str)
                        .context("Drive root ID is unavailable")?
                        .to_string(),
                }
            };
            let metadata = self
                .drive_request(account, &object.id, None, &[("fields", "parents".into())])
                .await?;
            let parents = metadata
                .get("parents")
                .and_then(Value::as_array)
                .context("Drive parent IDs are unavailable")?
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",");
            query.push(("addParents", parent_id));
            query.push(("removeParents", parents));
        }
        self.drive_request(account, &object.id, Some(json!({"name":name})), &query)
            .await?;
        Ok(())
    }
}

pub(crate) fn rooted(fs: &str, path: &str) -> String {
    if path.is_empty() {
        fs.to_string()
    } else {
        format!("{}{path}", fs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
    };

    #[tokio::test]
    async fn drive_mutations_and_copies_address_the_selected_id_not_its_duplicate_name() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stopped = stop.clone();
        let server = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                let request_line = headers.lines().next().unwrap().to_string();
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                recorded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((request_line.clone(), body));
                let result = if request_line.contains("/config/get") {
                    json!({"type":"drive", "token":"{\"access_token\":\"test-only\"}"})
                } else if request_line.contains("/backend/command") {
                    json!({"jobid":7})
                } else if request_line.starts_with("GET /files/") {
                    json!({"id":"object-one", "parents":["source-folder"]})
                } else {
                    json!({"id":"object-one"})
                };
                let result = result.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", result.len(), result).unwrap();
            }
        });
        let mut client = RcClient::new(&address.to_string(), "test", "test");
        client.drive_api = format!("http://{address}/files/");
        let parent = remote_path::join("", "source-folder", "Documents", true);
        let selected = remote_path::join(&parent, "object-one", "same.txt", false);
        let destination = remote_path::join("", "target-folder", "Other", true);
        let (fs, path) = client
            .scope("account-one", &format!("{destination}/renamed.txt"))
            .await
            .unwrap();
        assert_eq!(fs, "account-one,root_folder_id=target-folder:");
        assert_eq!(path, "renamed.txt");
        client
            .copy_id_async(
                "account-one",
                "object-one",
                "account-two:copy.txt",
                "test-job",
                None,
            )
            .await
            .unwrap();
        client
            .rename_id(
                "account-one",
                &selected,
                &format!("{destination}/renamed.txt"),
            )
            .await
            .unwrap();
        client.trash_id("account-one", &selected).await.unwrap();
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        let requests = requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let copy = requests
            .iter()
            .find(|(line, _)| line.contains("/backend/command"))
            .unwrap();
        assert_eq!(copy.1["arg"][0], "object-one");
        assert_eq!(copy.1["command"], "copyid");
        let patches: Vec<_> = requests
            .iter()
            .filter(|(line, _)| line.starts_with("PATCH"))
            .collect();
        assert_eq!(patches.len(), 2);
        assert!(patches
            .iter()
            .all(|(line, _)| line.contains("/files/object-one?")));
        assert!(patches[0].0.contains("addParents=target-folder"));
        assert!(patches[0].0.contains("removeParents=source-folder"));
        assert_eq!(patches[0].1["name"], "renamed.txt");
        assert_eq!(patches[1].1["trashed"], true);
        assert!(requests
            .iter()
            .all(|(line, _)| !line.contains("object-two")));
    }
}
