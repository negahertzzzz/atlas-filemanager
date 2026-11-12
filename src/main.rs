#![windows_subsystem = "windows"]

//! Atlas — thin launcher. The gpui application lives in the `atlas-ui` crate;
//! this binary only sets up logging, the tokio runtime, and hands off to
//! [`atlas_ui::run`].
// Panic-free stdio: std's print macros panic when the stream is a closed
// pipe, and a panic aborts the whole GUI process. These shadow the prelude
// macros for every module of this crate and drop write errors instead.
#[allow(unused_macros)]
macro_rules! println {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stdout(), $($arg)*);
    }};
}
#[allow(unused_macros)]
macro_rules! eprintln {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($arg)*);
    }};
}
#[allow(unused_macros)]
macro_rules! print {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::write!(::std::io::stdout(), $($arg)*);
    }};
}
#[allow(unused_macros)]
macro_rules! eprint {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::write!(::std::io::stderr(), $($arg)*);
    }};
}

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    #[cfg(windows)]
    if atlas_win_space::mft_space::helper::try_run() {
        return;
    }
    let version = option_env!("ATLAS_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    // Package smoke tests must verify the ELF and version without opening a window.
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--version")
    {
        println!("Atlas {version}");
        return;
    }

    #[cfg(all(windows, not(feature = "conversion-lab")))]
    if let Err(error) = atlas_win_space::crash::install_for(version) {
        eprintln!("failed to install crash handler: {error}");
    }

    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    let app_started = std::time::Instant::now();
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer().with_filter(tracing_subscriber::filter::filter_fn(
                |meta| *meta.level() <= tracing::Level::INFO,
            )),
        )
        .init();

    // Respect the processors available to this process, including affinity limits.
    // GPUI drives the UI on the main thread.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(std::thread::available_parallelism().map_or(1, usize::from))
        .max_blocking_threads(32)
        .thread_name("atlas-worker")
        .enable_all()
        .build();
    let Ok(rt) = rt else {
        tracing::error!("failed to create tokio runtime");
        return;
    };
    let handle = rt.handle().clone();
    std::thread::spawn(move || {
        let _rt = rt;
        loop {
            std::thread::park();
        }
    });

    #[cfg(feature = "dhat-heap")]
    let on_quit = Some(Box::new(move || {
        std::mem::drop(_profiler);
    }) as Box<dyn FnOnce() + 'static>);
    #[cfg(not(feature = "dhat-heap"))]
    let on_quit = None;

    let initial_paths = std::env::args_os().skip(1).map(Into::into).collect();
    atlas_ui::run(handle, app_started, on_quit, initial_paths);
    #[cfg(windows)]
    atlas_win_space::crash::mark_clean_exit();
}
