# Contributing

Atlas is a Rust/GPUI desktop file manager.

## Local Check

Before opening a PR or commit you care about, run:

```powershell
cargo fmt --check
cargo build --workspace --locked
$env:RUST_MIN_STACK = "16777216"
cargo test --workspace --locked --no-fail-fast
```

The debug translation initializer needs more than the default 2 MiB test-thread
stack. `RUST_MIN_STACK` gives tests 16 MiB, matching the Windows application's
main-thread stack. CI sets this variable on both Windows and Linux.

On Ubuntu 24.04, install the native build dependencies first:

```bash
sudo apt-get update
sudo apt-get install -y clang g++ pkg-config libasound2-dev libfontconfig-dev \
  libssl-dev libvulkan-dev libxcb1-dev libwayland-dev libx11-xcb-dev \
  libxkbcommon-x11-dev libbz2-dev liblzma-dev ffmpeg
cargo fmt --check
cargo build --workspace --locked
RUST_MIN_STACK=16777216 cargo test --workspace --locked --no-fail-fast
```

FFmpeg enables the independent subtitle-reader tests. Tests requiring the
optional cloud extension also need that extension and `rclone` installed;
ordinary workspace tests skip that integration when it is unavailable.

Strict clippy is tracked as cleanup work, but it is not a required gate yet:

```powershell
cargo clippy --workspace --all-targets -- -D warnings
```

## Build Profiles

Use `cargo devfast` for the smallest feature surface, `cargo devfull` when the
default archive/provider features are required, and `cargo rel` for the
production artifact. Both development commands and plain `cargo run` share the
same `target/debug` profile. Optional nightly compiler experiments and the
repeatable `atlas-ui` benchmark are documented in [docs/build-performance.md](docs/build-performance.md).

## Vendored Code

Atlas keeps modified GPUI and gpui-component sources under `vendor/`.
Changes to those forks should be committed in this repo with the app change that
needs them.
