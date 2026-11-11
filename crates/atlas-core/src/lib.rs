//! `atlas-core`: backends and background work — filesystem, search, cloud,
//! extensions, tag DB, watchers, jobs. Depends on `atlas-model`; never on the UI.
//!
//! Moved here from the legacy `src/{fs,cloud,ext,search,util}` trees (Phase J); the
//! binary re-exports each as `crate::{fs,cloud,ext,search,util}` via shims.
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


pub mod convert;
pub mod crash_report;
pub mod cleanup;
pub mod ext;
pub mod fs;
pub mod git;
pub mod license;
pub mod search;
pub mod space_lens;
pub mod update;
pub mod usage;
pub mod util;
pub mod vault;

#[cfg(feature = "provider-host")]
pub mod cloud;
