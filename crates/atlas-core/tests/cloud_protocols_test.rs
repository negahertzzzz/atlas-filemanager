use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use atlas_core::cloud::provider::CloudProvider;
use atlas_core::cloud::sidecar_provider::SidecarProvider;
use atlas_model::location::Location;
use serde_json::json;

struct ServerGuard {
    child: Child,
    _dir: tempfile::TempDir,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(proto: &str, port: u16, user: &str, pass: &str) -> (ServerGuard, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir for server");
    let base_path = dir.path().to_path_buf();

    // Create a subfolder with a file
    let docs = base_path.join("documents");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("readme.txt"), "hello from server").unwrap();

    let photos = base_path.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    std::fs::write(photos.join("sample.jpg"), "fake jpg bytes").unwrap();

    let mut cmd = Command::new("rclone");
    cmd.arg("serve")
        .arg(proto)
        .arg(&base_path)
        .arg("--addr")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--user")
        .arg(user)
        .arg("--pass")
        .arg(pass);

    let child = cmd.spawn().expect(&format!("spawn rclone serve {proto}"));

    // Give server a moment to start
    std::thread::sleep(Duration::from_millis(500));

    (ServerGuard { child, _dir: dir }, base_path)
}

#[tokio::test]
async fn test_cloud_ftp_webdav_sftp_navigation_and_operations() {
    let home = dirs::home_dir().expect("resolve home directory on this platform");
    let ext_dir = home.join(".local/share/atlas/extensions/cloud");
    if !ext_dir.join("extension.toml").exists() {
        eprintln!(
            "Skipping test: cloud extension not installed in ~/.local/share/atlas/extensions/cloud"
        );
        return;
    }

    let bin_name = if cfg!(windows) {
        "atlas-cloud-extension.exe"
    } else {
        "atlas-cloud-extension"
    };

    let provider: Arc<dyn CloudProvider> =
        Arc::new(SidecarProvider::spawn(&ext_dir, bin_name, &[]).expect("spawn sidecar provider"));

    // 1. FTP Server & Remote
    println!("=== Testing FTP ===");
    let ftp_port = 21210;
    let (_ftp_guard, _ftp_path) = spawn_server("ftp", ftp_port, "spito", "milox22");

    let ftp_id = "test-ftp-remote";
    let ftp_params = json!({
        "host": "127.0.0.1",
        "port": ftp_port.to_string(),
        "user": "spito",
        "pass": "milox22",
    });
    provider
        .configure(ftp_id, "ftp", ftp_params)
        .await
        .expect("configure FTP");

    // List FTP root
    let root_entries = provider.list(ftp_id, "").await.expect("list FTP root");
    println!(
        "FTP root entries: {:?}",
        root_entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert!(root_entries
        .iter()
        .any(|e| e.name == "documents" && e.is_dir));
    assert!(root_entries.iter().any(|e| e.name == "photos" && e.is_dir));

    // Navigate into documents
    let docs_entries = provider
        .list(ftp_id, "documents")
        .await
        .expect("list FTP documents");
    println!(
        "FTP documents entries: {:?}",
        docs_entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert!(docs_entries
        .iter()
        .any(|e| e.name == "readme.txt" && !e.is_dir));

    // Mkdir in FTP
    provider
        .mkdir(ftp_id, "documents/new_folder")
        .await
        .expect("mkdir in FTP");
    let docs_after_mkdir = provider.list(ftp_id, "documents").await.unwrap();
    assert!(docs_after_mkdir
        .iter()
        .any(|e| e.name == "new_folder" && e.is_dir));

    // Ascend test
    let loc = Location::Remote {
        id: ftp_id.to_string(),
        path: "documents/new_folder".to_string(),
    };
    let parent = loc.ascend().unwrap();
    assert_eq!(
        parent,
        Location::Remote {
            id: ftp_id.to_string(),
            path: "documents".to_string(),
        }
    );
    let root = parent.ascend().unwrap();
    assert_eq!(
        root,
        Location::Remote {
            id: ftp_id.to_string(),
            path: String::new(),
        }
    );
    assert_eq!(root.ascend(), None);

    provider.deconfigure(ftp_id).await.unwrap();
    println!("FTP test passed successfully!\n");

    // 2. WebDAV Server & Remote
    println!("=== Testing WebDAV ===");
    let webdav_port = 8888;
    let (_webdav_guard, _webdav_path) =
        spawn_server("webdav", webdav_port, "webdavuser", "davpass123");

    let webdav_id = "test-webdav-remote";
    let webdav_params = json!({
        "url": format!("http://127.0.0.1:{webdav_port}"),
        "vendor": "other",
        "user": "webdavuser",
        "pass": "davpass123",
    });
    provider
        .configure(webdav_id, "webdav", webdav_params)
        .await
        .expect("configure WebDAV");

    // List WebDAV root
    let dav_root_entries = provider
        .list(webdav_id, "")
        .await
        .expect("list WebDAV root");
    println!(
        "WebDAV root entries: {:?}",
        dav_root_entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert!(dav_root_entries
        .iter()
        .any(|e| e.name == "documents" && e.is_dir));

    // Navigate into documents
    let dav_docs_entries = provider
        .list(webdav_id, "documents")
        .await
        .expect("list WebDAV documents");
    println!(
        "WebDAV documents entries: {:?}",
        dav_docs_entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert!(dav_docs_entries
        .iter()
        .any(|e| e.name == "readme.txt" && !e.is_dir));

    // Mkdir in WebDAV
    provider
        .mkdir(webdav_id, "documents/dav_folder")
        .await
        .expect("mkdir in WebDAV");
    let dav_docs_after = provider.list(webdav_id, "documents").await.unwrap();
    assert!(dav_docs_after
        .iter()
        .any(|e| e.name == "dav_folder" && e.is_dir));

    provider.deconfigure(webdav_id).await.unwrap();
    println!("WebDAV test passed successfully!\n");

    // 3. SFTP Server & Remote
    println!("=== Testing SFTP ===");
    let sftp_port = 22220;
    let (_sftp_guard, _sftp_path) = spawn_server("sftp", sftp_port, "sftpuser", "sftppass123");

    let sftp_id = "test-sftp-remote";
    let sftp_params = json!({
        "host": "127.0.0.1",
        "port": sftp_port.to_string(),
        "user": "sftpuser",
        "pass": "sftppass123",
    });
    provider
        .configure(sftp_id, "sftp", sftp_params)
        .await
        .expect("configure SFTP");

    // List SFTP root
    let sftp_root_entries = provider.list(sftp_id, "").await.expect("list SFTP root");
    println!(
        "SFTP root entries: {:?}",
        sftp_root_entries
            .iter()
            .map(|e| &e.name)
            .collect::<Vec<_>>()
    );
    assert!(sftp_root_entries
        .iter()
        .any(|e| e.name == "documents" && e.is_dir));

    // Navigate into documents
    let sftp_docs_entries = provider
        .list(sftp_id, "documents")
        .await
        .expect("list SFTP documents");
    println!(
        "SFTP documents entries: {:?}",
        sftp_docs_entries
            .iter()
            .map(|e| &e.name)
            .collect::<Vec<_>>()
    );
    assert!(sftp_docs_entries
        .iter()
        .any(|e| e.name == "readme.txt" && !e.is_dir));

    // Mkdir in SFTP
    provider
        .mkdir(sftp_id, "documents/sftp_folder")
        .await
        .expect("mkdir in SFTP");
    let sftp_docs_after = provider.list(sftp_id, "documents").await.unwrap();
    assert!(sftp_docs_after
        .iter()
        .any(|e| e.name == "sftp_folder" && e.is_dir));

    provider.deconfigure(sftp_id).await.unwrap();
    println!("SFTP test passed successfully!\n");
}
