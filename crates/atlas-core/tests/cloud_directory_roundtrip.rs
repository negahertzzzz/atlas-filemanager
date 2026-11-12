//! Real RC API contract test using two isolated local aliases, never user accounts.
#![cfg(feature = "cloud-backend")]

use atlas_core::cloud::provider::{CloudProvider, ProgressSink};
use atlas_core::cloud::rclone::{rc_api::RcClient, RcloneProvider};
use serde_json::json;
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "requires rclone on PATH; uses only an isolated localhost daemon"]
async fn cloud_directories_round_trip_and_accounts_stay_isolated() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let first = root.path().join("account-one");
    let second = root.path().join("account-two");
    std::fs::create_dir_all(&first)?;
    std::fs::create_dir_all(&second)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    let mut command = Command::new("rclone");
    command
        .args([
            "rcd",
            "--rc-addr",
            &address.to_string(),
            "--rc-user",
            "atlas-test",
            "--rc-pass",
            "isolated-test",
        ])
        .arg("--config")
        .arg(root.path().join("rclone.conf"))
        .arg("--cache-dir")
        .arg(root.path().join("cache"))
        .arg("--log-file")
        .arg(root.path().join("rclone.log"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut daemon = Daemon(command.spawn()?);
    let rc = RcClient::new(&address.to_string(), "atlas-test", "isolated-test");
    let mut ready = false;
    for _ in 0..100 {
        if rc.version().await.is_ok() {
            ready = true;
            break;
        }
        if daemon.0.try_wait()?.is_some() {
            anyhow::bail!("isolated rclone exited during startup");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::ensure!(ready, "isolated rclone did not become ready");
    let provider = RcloneProvider::new(rc, root.path().join("cache"));
    provider
        .configure("account-one", "alias", json!({"remote": first}))
        .await?;
    provider
        .configure("account-two", "alias", json!({"remote": second}))
        .await?;

    let source = root.path().join("source");
    std::fs::create_dir_all(source.join("nested/empty"))?;
    std::fs::write(source.join("nested/unicode-á.txt"), b"nested content")?;
    std::fs::write(source.join("root.txt"), b"root content")?;
    std::fs::create_dir_all(first.join("backup"))?;
    std::fs::write(first.join("backup/existing.txt"), b"preserve destination")?;
    let progress: ProgressSink = Arc::new(|_, _| {});
    provider
        .upload("account-one", &source, "backup", progress.clone())
        .await?;
    assert_eq!(
        std::fs::read(first.join("backup/nested/unicode-á.txt"))?,
        b"nested content"
    );
    assert!(first.join("backup/nested/empty").is_dir());
    assert_eq!(
        std::fs::read(first.join("backup/existing.txt"))?,
        b"preserve destination"
    );
    assert!(
        provider.list("account-two", "").await?.is_empty(),
        "same provider must keep accounts isolated"
    );

    provider
        .copy("account-one", "backup", "server-copy")
        .await?;
    assert_eq!(
        std::fs::read(first.join("server-copy/root.txt"))?,
        b"root content"
    );
    assert!(first.join("server-copy/nested/empty").is_dir());
    let downloaded = root.path().join("downloaded");
    provider
        .download("account-one", "backup", &downloaded, progress.clone())
        .await?;
    assert_eq!(
        std::fs::read(downloaded.join("nested/unicode-á.txt"))?,
        b"nested content"
    );
    assert!(downloaded.join("nested/empty").is_dir());

    // Individual files keep using the single-file path.
    let local_file = source.join("root.txt");
    provider
        .upload("account-two", &local_file, "root.txt", progress.clone())
        .await?;
    let file_download = root.path().join("file-download.txt");
    provider
        .download("account-two", "root.txt", &file_download, progress.clone())
        .await?;
    assert_eq!(std::fs::read(file_download)?, b"root content");
    assert!(!second.join("backup").exists());
    provider
        .copy_between(
            "account-one",
            "backup",
            "account-two",
            "remote-copy",
            progress.clone(),
            atlas_core::cloud::control::TransferControl::default(),
        )
        .await?;
    assert_eq!(
        std::fs::read(second.join("remote-copy/nested/unicode-á.txt"))?,
        b"nested content"
    );
    assert!(second.join("remote-copy/nested/empty").is_dir());
    assert!(first.join("backup/root.txt").exists());

    // A later source revision must not silently overwrite an existing cloud object.
    std::fs::write(&local_file, b"different source revision")?;
    assert!(provider
        .upload("account-two", &local_file, "root.txt", progress.clone())
        .await
        .is_err());
    assert_eq!(std::fs::read(second.join("root.txt"))?, b"root content");

    let bounded_source = source.join("large.bin");
    std::fs::File::create(&bounded_source)?.set_len(1024 * 1024)?;
    provider
        .upload(
            "account-one",
            &bounded_source,
            "large.bin",
            progress.clone(),
        )
        .await?;
    let bounded_target = root.path().join("bounded.bin");
    let limit = atlas_core::cloud::control::TransferControl::default().with_byte_limit(Some(4096));
    assert!(provider
        .download_controlled(
            "account-one",
            "large.bin",
            &bounded_target,
            progress.clone(),
            limit
        )
        .await
        .is_err());
    assert!(
        !bounded_target.exists(),
        "a size-limited preview must never publish a partial download"
    );
    assert!(provider
        .download(
            "account-one",
            "missing",
            &root.path().join("missing"),
            progress
        )
        .await
        .is_err());
    assert!(provider
        .upload(
            "account-one",
            &PathBuf::from("missing-source"),
            "missing",
            Arc::new(|_, _| {})
        )
        .await
        .is_err());
    // Exercise the same bounded hydration queue used by the inspector, including
    // the real rclone transport and the built-in text decoder.
    let provider = Arc::new(provider);
    let (preview_tx, preview_rx) = flume::unbounded();
    let mut previews = atlas_core::cloud::preview::PreviewQueue::default();
    let key = PathBuf::from("remote-preview:account-one/test/root.txt");
    let tab = atlas_model::layout_tree::TileId(1);
    previews.request(
        &tokio::runtime::Handle::current(),
        provider.clone(),
        preview_tx,
        tab,
        "account-one".into(),
        "backup/root.txt".into(),
        key.clone(),
        Some(12),
        false,
    );
    let reply = tokio::time::timeout(Duration::from_secs(15), preview_rx.recv_async()).await??;
    match reply {
        atlas_model::app_state::BgResult::PreviewReady { path, result, .. } => {
            assert_eq!(path, key);
            assert!(
                matches!(&result, atlas_model::preview_cache::PreviewResult::Text { text, .. } if text == "root content"),
                "text hydration returned {result:?}"
            );
        }
        _ => anyhow::bail!("preview queue returned an unexpected event"),
    }
    provider.prepare_shutdown().await?;
    provider.deconfigure("account-one").await?;
    assert!(
        !provider.list("account-two", "").await?.is_empty(),
        "removing one account affected the other"
    );
    provider.deconfigure("account-two").await?;
    Ok(())
}
