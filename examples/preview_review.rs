//! Isolated native UI review launcher. Normal application code is shared with Atlas.
#![windows_subsystem = "windows"]

fn main() {
    let started = std::time::Instant::now();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    #[cfg(windows)]
    if std::env::var_os("ATLAS_PREVIEW_LAB").is_none() {
        let _ = atlas_win_space::crash::install();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .max_blocking_threads(32)
        .enable_all()
        .build()
        .expect("Atlas runtime");
    atlas_ui::run(
        runtime.handle().clone(),
        started,
        None,
        std::env::args_os().skip(1).map(Into::into).collect(),
    );
}
