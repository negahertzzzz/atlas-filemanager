//! The user's resource limit ("use at most N% of this PC").
//!
//! At 100% Atlas behaves as it always did. Below that:
//! - Windows enforces a hard CPU cap on the Atlas process and on the tools it
//!   starts for previews and conversions (see `atlas_win_space::resource_limit`);
//! - heavy work (disk analysis, copies, conversions, indexing) uses
//!   proportionally fewer threads, each in background mode, so disk and memory
//!   pressure drop too and the UI thread stays first in line.
//!
//! Helper processes (the cloud extension) inherit the limit through
//! [`ENV_VAR`].
use std::sync::atomic::{AtomicU8, Ordering};

/// Choices offered in Settings.
pub const CHOICES: [u8; 4] = [25, 50, 75, 100];
/// Environment variable that passes the limit to helper processes.
pub const ENV_VAR: &str = "ATLAS_CPU_LIMIT";

static PERCENT: AtomicU8 = AtomicU8::new(100);

/// Apply `percent` (clamped to 10–100) to this process. Takes effect at once
/// for the CPU cap and for every operation started afterwards.
pub fn apply(percent: u8) {
    let percent = percent.clamp(10, 100);
    let previous = PERCENT.swap(percent, Ordering::Relaxed);
    if previous == percent && percent == 100 {
        return;
    }
    #[cfg(windows)]
    if let Err(error) = atlas_win_space::resource_limit::set_cpu_cap(percent) {
        tracing::warn!(%error, percent, "cannot apply the CPU limit; thread priorities still apply");
    }
    tracing::info!(percent, "resource limit");
}

/// Apply the limit a parent Atlas process passed in [`ENV_VAR`].
pub fn apply_from_env() {
    if let Some(percent) = std::env::var(ENV_VAR)
        .ok()
        .and_then(|value| value.trim().parse().ok())
    {
        apply(percent);
    }
}

/// The current limit in percent (100 = unlimited).
pub fn percent() -> u8 {
    PERCENT.load(Ordering::Relaxed)
}

pub fn limited() -> bool {
    percent() < 100
}

/// Logical processors available to Atlas.
pub fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// Scale a worker count chosen for full power down to the limit, rounding
/// up and never below one.
pub fn scaled(full: usize) -> usize {
    scale(full, percent())
}

const fn scale(full: usize, percent: u8) -> usize {
    let scaled = (full * percent as usize).div_ceil(100);
    if scaled == 0 {
        1
    } else {
        scaled
    }
}

/// For thread-pool start handlers: heavy workers run in background mode
/// while a limit is set.
pub fn lower_current_thread() {
    #[cfg(windows)]
    if limited() {
        atlas_win_space::resource_limit::background_thread(true);
    }
}

/// Background mode for the current thread until this value drops, when a
/// limit is set. For jobs on shared pools whose threads outlive the job; the
/// thread is restored even if the job panics.
#[must_use = "background mode ends when the guard drops"]
pub struct Background(bool);

impl Background {
    pub fn enter() -> Self {
        #[cfg(windows)]
        if limited() {
            atlas_win_space::resource_limit::background_thread(true);
            return Self(true);
        }
        Self(false)
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.0 {
            atlas_win_space::resource_limit::background_thread(false);
        }
    }
}

/// Windows process creation flags for a tool Atlas starts: `base`, plus
/// below-normal priority while a limit is set.
pub fn tool_creation_flags(base: u32) -> u32 {
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    if limited() {
        base | BELOW_NORMAL_PRIORITY_CLASS
    } else {
        base
    }
}

/// Put a tool process Atlas just started under the CPU cap.
pub fn contain_child(child: &std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        atlas_win_space::resource_limit::contain(child.as_raw_handle() as isize);
    }
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(test)]
mod tests {
    use super::scale;

    #[test]
    fn worker_counts_scale_down_but_never_reach_zero() {
        assert_eq!(scale(16, 100), 16);
        assert_eq!(scale(16, 50), 8);
        assert_eq!(scale(16, 25), 4);
        assert_eq!(scale(2, 25), 1);
        assert_eq!(scale(1, 10), 1);
        assert_eq!(scale(0, 50), 1);
    }
}
