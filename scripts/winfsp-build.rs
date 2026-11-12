// Resolve WinFsp only on first mount so Atlas still starts without its driver.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The 1 MiB default main-thread stack overflows inside deep gpui layout
    // recursion (debug builds reliably, release builds on big trees). The
    // reservation is virtual; only touched pages are committed. This script is
    // shared with lib-only crates, where `-bins` link args are rejected.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        && std::env::var("CARGO_PKG_NAME").as_deref() == Ok("atlas")
    {
        println!("cargo:rustc-link-arg-bins=/STACK:16777216");
        // Embed only the PDB's file name, not the build machine's path.
        println!("cargo:rustc-link-arg-bins=/PDBALTPATH:%_PDB%");
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let dll = match arch.as_str() {
        "x86_64" => "winfsp-x64.dll",
        "aarch64" => "winfsp-a64.dll",
        "x86" => "winfsp-x86.dll",
        _ => return,
    };
    println!("cargo:rustc-link-lib=dylib=delayimp");
    // Standalone examples need not reference the package's library. Cargo only
    // forwards rustc-link-lib through that library when one exists, so also
    // pass the helper directly to every target receiving /DELAYLOAD.
    println!("cargo:rustc-link-arg=delayimp.lib");
    println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
}
