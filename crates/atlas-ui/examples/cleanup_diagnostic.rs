//! Hidden-window GPU/interaction lab. With windows-cleanup enabled, deletion is
//! tested exclusively on files created by the harness in a unique Temp folder.
fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    if atlas_win_space::mft_space::helper::try_run() {
        return Ok(());
    }
    let output = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .expect("pass a diagnostic output directory");
    // Debug builds of deeply nested GPUI trees exceed the default 1 MiB main stack.
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || atlas_ui::ui::cleanup::diagnostic::run(output))?
        .join()
        .map_err(|_| anyhow::anyhow!("lab panicked"))?
}
