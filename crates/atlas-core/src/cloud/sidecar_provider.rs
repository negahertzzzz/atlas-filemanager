use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use atlas_model::provider_protocol::{
    EntryRef, EventPayload, Operation, ProviderEntry, RemoteConfig, ResultPayload,
};

use super::control::{OutcomeUnknown, TransferCancelled, TransferControl};
use super::provider::{Capabilities, CloudProvider, ProgressSink, RemoteEntry};
use crate::ext::provider_sidecar::ProviderSidecar;

pub struct SidecarProvider {
    sidecar: Arc<ProviderSidecar>,
    progress: Arc<Mutex<HashMap<String, ProgressSink>>>,
}

impl SidecarProvider {
    pub fn spawn(extension_dir: &Path, command: &str, args: &[String]) -> anyhow::Result<Self> {
        Self::spawn_with_progress(extension_dir, command, args, None)
    }

    pub fn spawn_with_progress(
        extension_dir: &Path,
        command: &str,
        args: &[String],
        startup_progress: Option<ProgressSink>,
    ) -> anyhow::Result<Self> {
        let (event_tx, event_rx) = flume::unbounded();
        let sidecar = Arc::new(ProviderSidecar::spawn(
            extension_dir,
            command,
            args,
            event_tx,
        )?);
        let progress: Arc<Mutex<HashMap<String, ProgressSink>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let event_progress = progress.clone();
        std::thread::Builder::new()
            .name("atlas-provider-events".into())
            .spawn(move || {
                while let Ok(event) = event_rx.recv() {
                    if let EventPayload::Progress {
                        transfer_id,
                        done,
                        total,
                    } = event.event
                    {
                        if transfer_id == "rclone-download" {
                            if let Some(progress) = &startup_progress {
                                progress(done, total);
                            }
                            continue;
                        }
                        if let Some(sink) = event_progress
                            .lock()
                            .or_else(atlas_model::sync::recover)
                            .ok()
                            .and_then(|sinks| sinks.get(&transfer_id).cloned())
                        {
                            sink(done, total);
                        }
                    }
                }
            })?;
        let provider = Self { sidecar, progress };
        let initialized = provider
            .sidecar
            .request(Operation::Initialize {
                data_dir: extension_dir.join("data").to_string_lossy().into_owned(),
            })
            .context("Could not initialize the Cloud extension. Reinstall Cloud Drives or update Atlas and its Cloud extension together")?;
        Self::decode(initialized)?;
        Ok(provider)
    }

    async fn call(&self, operation: Operation) -> anyhow::Result<serde_json::Value> {
        Self::decode(self.sidecar.request_async(operation, None).await?)
    }

    fn decode(
        response: atlas_model::provider_protocol::Response,
    ) -> anyhow::Result<serde_json::Value> {
        match response.result {
            ResultPayload::Ok { value } => Ok(value),
            ResultPayload::Error { message, code } => match code.as_deref() {
                Some("cancelled") => Err(TransferCancelled.into()),
                Some("outcome_unknown") => Err(OutcomeUnknown(message).into()),
                _ => anyhow::bail!(message),
            },
        }
    }

    async fn transfer(
        &self,
        id: String,
        operation: Operation,
        sink: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        self.progress
            .lock()
            .map_err(|_| anyhow::anyhow!("provider progress map poisoned"))?
            .insert(id.clone(), sink);
        let result = self
            .sidecar
            .request_async(operation, Some(control))
            .await
            .and_then(Self::decode)
            .map(|_| ());
        self.progress
            .lock()
            .or_else(atlas_model::sync::recover)
            .ok()
            .map(|mut sinks| sinks.remove(&id));
        result
    }
}

#[async_trait::async_trait]
impl CloudProvider for SidecarProvider {
    async fn prepare_shutdown(&self) -> anyhow::Result<()> {
        self.call(Operation::PrepareShutdown).await.map(|_| ())
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_mount: true,
            can_write: true,
            random_read: false,
        }
    }

    async fn configure(
        &self,
        id: &str,
        backend_type: &str,
        parameters: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.call(Operation::Configure {
            remote: RemoteConfig {
                id: id.into(),
                name: id.into(),
                kind: backend_type.into(),
                options: parameters,
            },
            secret: String::new(),
        })
        .await
        .map(|_| ())
    }

    async fn configure_oauth(
        &self,
        id: &str,
        backend_type: &str,
        parameters: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.configure(id, backend_type, parameters).await
    }

    async fn deconfigure(&self, id: &str) -> anyhow::Result<()> {
        self.call(Operation::RemoveRemote {
            remote_id: id.into(),
        })
        .await
        .map(|_| ())
    }

    async fn list(&self, id: &str, path: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        let value = self
            .call(Operation::List {
                remote_id: id.into(),
                path: path.into(),
            })
            .await?;
        serde_json::from_value::<Vec<ProviderEntry>>(value)?
            .into_iter()
            .map(|entry| {
                Ok(RemoteEntry {
                    name: entry.name,
                    path: entry.path,
                    is_dir: entry.is_dir,
                    size: entry.size.unwrap_or(0),
                    modified: entry.modified_unix_ms.map(|ms| ms / 1000),
                    mime: entry.mime,
                })
            })
            .collect()
    }

    async fn download_controlled(
        &self,
        id: &str,
        src: &str,
        dst: &Path,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        let transfer_id = uuid::Uuid::new_v4().to_string();
        self.transfer(
            transfer_id.clone(),
            Operation::Download {
                transfer_id,
                remote_id: id.into(),
                source: src.into(),
                destination: dst.to_string_lossy().into_owned(),
                byte_limit: control.byte_limit(),
            },
            progress,
            control,
        )
        .await
    }

    async fn upload_controlled(
        &self,
        id: &str,
        src: &Path,
        dst: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        let transfer_id = uuid::Uuid::new_v4().to_string();
        self.transfer(
            transfer_id.clone(),
            Operation::Upload {
                transfer_id,
                remote_id: id.into(),
                source: src.to_string_lossy().into_owned(),
                destination: dst.into(),
                byte_limit: control.byte_limit(),
            },
            progress,
            control,
        )
        .await
    }

    async fn about(&self, id: &str) -> anyhow::Result<(u64, u64)> {
        let value = self
            .call(Operation::Quota {
                remote_id: id.into(),
            })
            .await?;
        Ok((
            value["used"].as_u64().unwrap_or(0),
            value["total"].as_u64().unwrap_or(0),
        ))
    }

    async fn copy_between(
        &self,
        source_id: &str,
        source: &str,
        destination_id: &str,
        destination: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        let transfer_id = uuid::Uuid::new_v4().to_string();
        self.transfer(
            transfer_id.clone(),
            Operation::CopyBetween {
                transfer_id,
                source_id: source_id.into(),
                source: source.into(),
                destination_id: destination_id.into(),
                destination: destination.into(),
            },
            progress,
            control,
        )
        .await
    }

    async fn mkdir(&self, id: &str, path: &str) -> anyhow::Result<()> {
        self.call(Operation::CreateDirectory {
            remote_id: id.into(),
            path: path.into(),
        })
        .await
        .map(|_| ())
    }

    async fn delete(&self, id: &str, path: &str, is_dir: bool) -> anyhow::Result<()> {
        self.call(Operation::Delete {
            remote_id: id.into(),
            entries: vec![EntryRef {
                path: path.into(),
                is_dir,
            }],
        })
        .await
        .map(|_| ())
    }

    async fn rename(&self, id: &str, from: &str, to: &str) -> anyhow::Result<()> {
        self.call(Operation::Rename {
            remote_id: id.into(),
            from: from.into(),
            to: to.into(),
        })
        .await
        .map(|_| ())
    }

    async fn copy(&self, id: &str, from: &str, to: &str) -> anyhow::Result<()> {
        self.call(Operation::CopyObject {
            remote_id: id.into(),
            from: from.into(),
            to: to.into(),
        })
        .await
        .map(|_| ())
    }

    async fn mount(&self, id: &str, mount_point: &Path) -> anyhow::Result<()> {
        self.call(Operation::Mount {
            remote_id: id.into(),
            mount_point: mount_point.to_string_lossy().into_owned(),
        })
        .await
        .map(|_| ())
    }

    async fn unmount(&self, mount_point: &Path) -> anyhow::Result<()> {
        self.call(Operation::Unmount {
            remote_id: String::new(),
            mount_point: mount_point.to_string_lossy().into_owned(),
        })
        .await
        .map(|_| ())
    }

    async fn install_mount_support(&self, progress: ProgressSink) -> anyhow::Result<()> {
        let transfer_id = uuid::Uuid::new_v4().to_string();
        self.progress
            .lock()
            .map_err(|_| anyhow::anyhow!("provider progress map poisoned"))?
            .insert(transfer_id.clone(), progress);
        let result = self
            .call(Operation::InstallMountSupport {
                transfer_id: transfer_id.clone(),
            })
            .await
            .map(|_| ());
        self.progress
            .lock()
            .or_else(atlas_model::sync::recover)
            .ok()
            .map(|mut sinks| sinks.remove(&transfer_id));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[ignore = "requires ATLAS_CLOUD_EXTENSION_DIR with an installed provider"]
    fn real_cloud_sidecar_round_trip_when_enabled() {
        let Some(dir) = std::env::var_os("ATLAS_CLOUD_EXTENSION_DIR").map(PathBuf::from) else {
            return;
        };
        let provider = SidecarProvider::spawn(
            &dir,
            if cfg!(windows) {
                "atlas-cloud-extension.exe"
            } else {
                "atlas-cloud-extension"
            },
            &[],
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(provider.list("atlas-test-missing", ""))
            .unwrap_err();
        assert!(!error.to_string().is_empty());
    }
}
