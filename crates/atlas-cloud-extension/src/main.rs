#![windows_subsystem = "windows"]

use anyhow::{Context as _, Result};
use atlas_core::cloud::control::{OutcomeUnknown, TransferCancelled, TransferControl};
use atlas_core::cloud::provider::{CloudProvider, ProgressSink};
use atlas_core::cloud::rclone::{rc_api::RcClient, sidecar, RcloneProvider};
use atlas_model::provider_protocol::{
    Event, EventPayload, Operation, ProviderEntry, Request, Response, ResultPayload, VERSION,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
struct Runtime {
    provider: Arc<dyn CloudProvider>,
    output: Arc<Mutex<std::io::Stdout>>,
    active: Arc<Mutex<HashMap<u64, ActiveRequest>>>,
    configuration_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
struct ActiveRequest {
    control: TransferControl,
    done: tokio::sync::watch::Receiver<bool>,
}

#[tokio::main]
async fn main() -> Result<()> {
    atlas_core::resources::apply_from_env();
    let output = Arc::new(Mutex::new(std::io::stdout()));
    let (runtime, _daemon) = start_provider(output.clone()).await?;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) if request.version == VERSION => request,
            Ok(request) => {
                write_response(
                    &output,
                    response_error(request.id, "unsupported provider protocol version"),
                )?;
                continue;
            }
            Err(error) => {
                eprintln!("invalid request: {error}");
                continue;
            }
        };
        let id = request.id;
        if matches!(request.operation, Operation::Shutdown) {
            write_response(&output, response_ok(id, Value::Null))?;
            break;
        }
        let runtime = runtime.clone();
        let output = output.clone();
        if let Operation::CancelRequest { request_id } = request.operation {
            tokio::spawn(async move {
                let active = runtime
                    .active
                    .lock()
                    .ok()
                    .and_then(|active| active.get(&request_id).cloned());
                if let Some(mut active) = active {
                    active.control.cancel();
                    let _ = active.done.wait_for(|done| *done).await;
                }
                let _ = write_response(&output, response_ok(id, Value::Null));
            });
            continue;
        }
        let control = TransferControl::default();
        let (done_tx, done) = tokio::sync::watch::channel(false);
        runtime
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("request registry poisoned"))?
            .insert(
                id,
                ActiveRequest {
                    control: control.clone(),
                    done,
                },
            );
        tokio::spawn(async move {
            let execution_runtime = runtime.clone();
            let joined = tokio::spawn(async move {
                execute(&execution_runtime, request.operation, control).await
            })
            .await;
            let response = match joined {
                Ok(Ok(value)) => response_ok(id, value),
                Ok(Err(error)) => Response {
                    version: VERSION,
                    id,
                    result: ResultPayload::Error {
                        message: format!("{error:#}"),
                        code: if error.is::<TransferCancelled>() {
                            Some("cancelled".into())
                        } else if error.is::<OutcomeUnknown>() {
                            Some("outcome_unknown".into())
                        } else {
                            None
                        },
                    },
                },
                Err(error) => Response {
                    version: VERSION,
                    id,
                    result: ResultPayload::Error {
                        message: format!(
                            "Provider task terminated before confirming its outcome: {error}"
                        ),
                        code: Some("outcome_unknown".into()),
                    },
                },
            };
            let _ = write_response(&output, response);
            done_tx.send_replace(true);
            if let Ok(mut active) = runtime.active.lock() {
                active.remove(&id);
            }
        });
    }
    Ok(())
}

async fn start_provider(output: Arc<Mutex<std::io::Stdout>>) -> Result<(Runtime, sidecar::Daemon)> {
    let progress_output = output.clone();
    let binary = sidecar::ensure_binary(Box::new(move |done, total| {
        let _ = write_event(
            &progress_output,
            EventPayload::Progress {
                transfer_id: "rclone-download".into(),
                done,
                total,
            },
        );
    }))
    .await?;
    let daemon = sidecar::spawn_daemon(&binary)?;
    let rc = RcClient::new(&daemon.addr, &daemon.user, &daemon.pass);
    for _ in 0..40 {
        if rc.version().await.is_ok() {
            rc.list_remotes()
                .await
                .context("load provider configuration before concurrent requests")?;
            let cache = std::env::current_dir()?.join("cache");
            return Ok((
                Runtime {
                    provider: Arc::new(RcloneProvider::new(rc, cache)),
                    output,
                    active: Arc::default(),
                    configuration_lock: Arc::default(),
                },
                daemon,
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("rclone daemon did not become ready")
}

async fn execute(
    runtime: &Runtime,
    operation: Operation,
    control: TransferControl,
) -> Result<Value> {
    let provider = &runtime.provider;
    match operation {
        Operation::PrepareShutdown => {
            provider.prepare_shutdown().await?;
            Ok(Value::Null)
        }
        Operation::Initialize { data_dir } => {
            std::fs::create_dir_all(data_dir)?;
            Ok(Value::Null)
        }
        Operation::Configure { remote, secret } => {
            let _configuration_guard = runtime.configuration_lock.lock().await;
            let mut options = remote.options;
            if !secret.is_empty() {
                options["pass"] = Value::String(secret);
            }
            if matches!(remote.kind.as_str(), "drive" | "onedrive") {
                provider
                    .configure_oauth(&remote.id, &remote.kind, options)
                    .await?;
            } else {
                provider
                    .configure(&remote.id, &remote.kind, options)
                    .await?;
            }
            Ok(Value::Null)
        }
        Operation::RemoveRemote { remote_id } => {
            let _configuration_guard = runtime.configuration_lock.lock().await;
            provider.deconfigure(&remote_id).await?;
            Ok(Value::Null)
        }
        Operation::List { remote_id, path } => {
            let entries = tokio::select! {
                result = provider.list(&remote_id, &path) => result?,
                () = control.cancelled() => return Err(TransferCancelled.into()),
            };
            let entries: Vec<ProviderEntry> = entries
                .into_iter()
                .map(|entry| ProviderEntry {
                    name: entry.name,
                    path: entry.path,
                    is_dir: entry.is_dir,
                    size: Some(entry.size),
                    modified_unix_ms: entry.modified.map(|seconds| seconds * 1000),
                    mime: entry.mime,
                })
                .collect();
            Ok(serde_json::to_value(entries)?)
        }
        Operation::Stat { remote_id, path } => {
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", &path));
            let entries = tokio::select! {
                result = provider.list(&remote_id, parent) => result?,
                () = control.cancelled() => return Err(TransferCancelled.into()),
            };
            let entry = entries
                .into_iter()
                .find(|entry| {
                    if atlas_model::remote_path::last(&path).is_some() {
                        entry.path == path
                    } else {
                        entry.name == name
                    }
                })
                .context("remote entry not found")?;
            Ok(json!(ProviderEntry {
                name: entry.name,
                path: entry.path,
                is_dir: entry.is_dir,
                size: Some(entry.size),
                modified_unix_ms: entry.modified.map(|seconds| seconds * 1000),
                mime: entry.mime,
            }))
        }
        Operation::Quota { remote_id } => {
            let (used, total) = provider.about(&remote_id).await?;
            Ok(json!({ "used": used, "total": total }))
        }
        Operation::InstallMountSupport { transfer_id } => {
            provider
                .install_mount_support(progress(runtime, &transfer_id))
                .await?;
            Ok(Value::Null)
        }
        Operation::CreateDirectory { remote_id, path } => {
            provider.mkdir(&remote_id, &path).await?;
            Ok(Value::Null)
        }
        Operation::Delete { remote_id, entries } => {
            for entry in entries {
                provider
                    .delete(&remote_id, &entry.path, entry.is_dir)
                    .await?;
            }
            Ok(Value::Null)
        }
        Operation::Rename {
            remote_id,
            from,
            to,
        } => {
            provider.rename(&remote_id, &from, &to).await?;
            Ok(Value::Null)
        }
        Operation::Copy {
            remote_id,
            entries,
            destination,
        } => {
            for entry in entries {
                let name = atlas_model::remote_path::file_name(&entry.path);
                let target = if destination.is_empty() {
                    name
                } else {
                    format!("{}/{name}", destination.trim_end_matches('/'))
                };
                provider.copy(&remote_id, &entry.path, &target).await?;
            }
            Ok(Value::Null)
        }
        Operation::CopyObject {
            remote_id,
            from,
            to,
        } => {
            provider.copy(&remote_id, &from, &to).await?;
            Ok(Value::Null)
        }
        Operation::CopyBetween {
            transfer_id,
            source_id,
            source,
            destination_id,
            destination,
        } => {
            provider
                .copy_between(
                    &source_id,
                    &source,
                    &destination_id,
                    &destination,
                    progress(runtime, &transfer_id),
                    control,
                )
                .await?;
            Ok(Value::Null)
        }
        Operation::Download {
            transfer_id,
            remote_id,
            source,
            destination,
            byte_limit,
        } => {
            provider
                .download_controlled(
                    &remote_id,
                    &source,
                    Path::new(&destination),
                    progress(runtime, &transfer_id),
                    control.with_byte_limit(byte_limit),
                )
                .await?;
            Ok(Value::Null)
        }
        Operation::Upload {
            transfer_id,
            remote_id,
            source,
            destination,
            byte_limit,
        } => {
            provider
                .upload_controlled(
                    &remote_id,
                    Path::new(&source),
                    &destination,
                    progress(runtime, &transfer_id),
                    control.with_byte_limit(byte_limit),
                )
                .await?;
            Ok(Value::Null)
        }
        Operation::Mount {
            remote_id,
            mount_point,
        } => {
            provider.mount(&remote_id, Path::new(&mount_point)).await?;
            Ok(Value::Null)
        }
        Operation::Unmount {
            remote_id: _,
            mount_point,
        } => {
            provider.unmount(Path::new(&mount_point)).await?;
            Ok(Value::Null)
        }
        Operation::Shutdown => Ok(Value::Null),
        Operation::CancelRequest { .. } => unreachable!("handled before dispatch"),
    }
}

fn progress(runtime: &Runtime, transfer_id: &str) -> ProgressSink {
    let output = runtime.output.clone();
    let transfer_id = transfer_id.to_string();
    Arc::new(move |done, total| {
        let _ = write_event(
            &output,
            EventPayload::Progress {
                transfer_id: transfer_id.clone(),
                done,
                total,
            },
        );
    })
}

fn response_ok(id: u64, value: Value) -> Response {
    Response {
        version: VERSION,
        id,
        result: ResultPayload::Ok { value },
    }
}

fn response_error(id: u64, message: &str) -> Response {
    Response {
        version: VERSION,
        id,
        result: ResultPayload::Error {
            message: message.into(),
            code: None,
        },
    }
}

fn write_response(output: &Arc<Mutex<std::io::Stdout>>, response: Response) -> Result<()> {
    write_json(output, &response)
}

fn write_event(output: &Arc<Mutex<std::io::Stdout>>, event: EventPayload) -> Result<()> {
    write_json(
        output,
        &Event {
            version: VERSION,
            event,
        },
    )
}

fn write_json(output: &Arc<Mutex<std::io::Stdout>>, value: &impl serde::Serialize) -> Result<()> {
    let mut output = output
        .lock()
        .map_err(|_| anyhow::anyhow!("stdout lock poisoned"))?;
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
