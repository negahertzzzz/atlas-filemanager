#[derive(Clone, Copy, Debug)]
pub(crate) enum UiLabAction {
    SetDetails,
    SplitHorizontal,
    ClosePane,
    SetGrid,
    Quit,
}

#[cfg(any(feature = "perf-log", feature = "tracy"))]
mod imp {
    use std::collections::HashMap;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    fn log(args: std::fmt::Arguments<'_>) {
        // A redirected console can close while Atlas is still rendering.
        // Diagnostics must not panic (especially from Span::drop).
        let _ = std::io::stderr().lock().write_fmt(format_args!("{args}\n"));
    }

    struct Config {
        enabled: bool,
        events: bool,
        threshold: Duration,
        started: Instant,
    }

    static CONFIG: OnceLock<Config> = OnceLock::new();
    static FRAME: AtomicU64 = AtomicU64::new(0);
    static DOCK_FRAME: AtomicU64 = AtomicU64::new(0);
    static UI_LAB_NEXT_ACTION: AtomicU64 = AtomicU64::new(0);
    static COUNTERS: OnceLock<Mutex<HashMap<&'static str, u64>>> = OnceLock::new();
    /// Wall-time accumulators (microseconds) for the current frame.
    /// Spans always add here even when under the log threshold, so N×0.3ms panes are visible.
    static TIMINGS_US: OnceLock<Mutex<HashMap<&'static str, u64>>> = OnceLock::new();
    static LAST_FRAME: OnceLock<Mutex<Instant>> = OnceLock::new();
    static LAST_FRAME_DETAIL: OnceLock<Mutex<Option<String>>> = OnceLock::new();

    fn truthy(name: &str) -> bool {
        std::env::var(name)
            .ok()
            .is_some_and(|v| !matches!(v.as_str(), "" | "0" | "false" | "FALSE" | "off" | "OFF"))
    }

    fn config() -> &'static Config {
        CONFIG.get_or_init(|| {
            let enabled = truthy("ATLAS_PERF");
            let events = truthy("ATLAS_PERF_EVENTS");
            let threshold_ms = std::env::var("ATLAS_PERF_MS")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(4.0)
                .max(0.0);
            Config {
                enabled: enabled || events,
                events,
                threshold: Duration::from_secs_f64(threshold_ms / 1000.0),
                started: Instant::now(),
            }
        })
    }

    pub(crate) fn next_frame() -> u64 {
        #[cfg(feature = "tracy")]
        tracy_client::frame_mark();

        if config().enabled {
            FRAME.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            0
        }
    }

    pub(crate) fn next_dock_frame() -> u64 {
        if config().enabled {
            DOCK_FRAME.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            0
        }
    }

    pub(crate) fn count(name: &'static str) {
        count_by(name, 1);
    }

    pub(crate) fn count_by(name: &'static str, amount: u64) {
        if !config().enabled {
            return;
        }
        let mut counters = COUNTERS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *counters.entry(name).or_default() += amount;
    }

    /// Accumulate microseconds under a stable name (summed across all call sites this frame).
    pub(crate) fn time_us(name: &'static str, us: u64) {
        if !config().enabled || us == 0 {
            return;
        }
        let mut timings = TIMINGS_US
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *timings.entry(name).or_default() += us;
    }

    /// RAII scope that always feeds `time_us` (and optionally logs if over threshold).
    pub(crate) fn span(name: &'static str) -> Option<Span> {
        (cfg!(feature = "tracy") || config().enabled).then(|| Span {
            name,
            detail: None,
            start: Instant::now(),
            #[cfg(feature = "tracy")]
            _tracy: tracy_client::Client::start().span_alloc(Some(name), name, file!(), line!(), 0),
        })
    }

    pub(crate) fn span_detail(name: &'static str, detail: impl FnOnce() -> String) -> Option<Span> {
        (cfg!(feature = "tracy") || config().enabled).then(|| {
            let detail = config().enabled.then(detail);
            Span {
                name,
                detail,
                start: Instant::now(),
                #[cfg(feature = "tracy")]
                _tracy: tracy_client::Client::start().span_alloc(
                    Some(name),
                    name,
                    file!(),
                    line!(),
                    0,
                ),
            }
        })
    }

    pub(crate) fn frame_summary(frame: u64, detail: impl FnOnce() -> String) {
        let cfg = config();
        if !cfg.events {
            return;
        }
        // App::render runs before GPUI prepaint/paint, so these counters and
        // timings describe the previous completed App frame. Keep its detail
        // next to the accumulators instead of labeling it with current state.
        let (summary_frame, detail_s) = {
            let mut previous = LAST_FRAME_DETAIL
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match previous.replace(detail()) {
                Some(previous) => (frame.saturating_sub(1), previous),
                None => (frame, previous.take().expect("current frame detail")),
            }
        };
        let now = Instant::now();
        let gap_ms = {
            let mut last = LAST_FRAME
                .get_or_init(|| Mutex::new(now))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let gap = now.duration_since(*last).as_secs_f64() * 1000.0;
            *last = now;
            gap
        };
        let counters = COUNTERS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .collect::<Vec<_>>();
        let timings = TIMINGS_US
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .collect::<Vec<_>>();
        if counters.is_empty() && timings.is_empty() && frame > 1 {
            return;
        }
        let mut counters = counters;
        counters.sort_by_key(|(name, _)| *name);
        let mut timings = timings;
        // Emit largest time consumers first for human scanning.
        timings.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let counter_s = counters
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ");
        // us -> ms with 2 decimals for readability in frame_summary.
        let timing_s = timings
            .into_iter()
            .map(|(name, us)| format!("{name}_ms={:.2}", us as f64 / 1000.0))
            .collect::<Vec<_>>()
            .join(" ");
        log(format_args!(
            "[atlas-perf +{:>8.1}ms] frame_summary frame={summary_frame} sampled_at={frame} gap_ms={gap_ms:.1} {} {} {}",
            cfg.started.elapsed().as_secs_f64() * 1000.0,
            detail_s,
            counter_s,
            timing_s
        ));
    }

    pub(crate) fn event(detail: impl FnOnce() -> String) {
        let cfg = config();
        if cfg.events {
            log(format_args!(
                "[atlas-perf +{:>8.1}ms] {}",
                cfg.started.elapsed().as_secs_f64() * 1000.0,
                detail()
            ));
        }
    }

    /// Isolation flags for A/B: skip chrome so gap reflects dock/panes only.
    pub(crate) fn skip_sidebar() -> bool {
        truthy("ATLAS_PERF_SKIP_SIDEBAR")
    }

    pub(crate) fn skip_overlays() -> bool {
        truthy("ATLAS_PERF_SKIP_OVERLAYS")
    }

    pub(crate) fn skip_inspector() -> bool {
        truthy("ATLAS_PERF_SKIP_INSPECTOR")
    }

    /// Deterministic pane-animation workload for A/B measurements. It is compiled
    /// only into perf/tracy builds and inert unless explicitly enabled.
    pub(crate) fn ui_lab_active() -> bool {
        truthy("ATLAS_PERF_AUTOMATE_SPLITS")
    }

    pub(crate) fn ui_lab_action() -> Option<super::UiLabAction> {
        if !ui_lab_active() {
            return None;
        }

        const START_DELAY_MS: u64 = 800;
        // Stress runs can interrupt the 240/180 ms transitions without changing
        // the ordinary UI-lab sequence or its default timing.
        static SEQUENCE: OnceLock<(u64, u64)> = OnceLock::new();
        let &(action_interval_ms, splits) = SEQUENCE.get_or_init(|| {
            let bounded = |key, default, min, max| {
                std::env::var(key)
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(default)
                    .clamp(min, max)
            };
            (
                bounded("ATLAS_PERF_SPLIT_INTERVAL_MS", 420, 16, 2000),
                bounded("ATLAS_PERF_SPLIT_COUNT", 12, 1, 64),
            )
        });
        let set_grid_stage = 1 + splits * 2;
        let quit_stage = set_grid_stage + 1;

        let elapsed_ms = config().started.elapsed().as_millis() as u64;
        if elapsed_ms < START_DELAY_MS {
            return None;
        }
        let due_stage = (elapsed_ms - START_DELAY_MS) / action_interval_ms;
        let next = UI_LAB_NEXT_ACTION.load(Ordering::Relaxed);
        if next > due_stage || next > quit_stage {
            return None;
        }
        // Let the final close settle and the teardown timer drain before the
        // Grid/Quit stages, including when actions arrive faster than motion.
        if next == set_grid_stage
            && elapsed_ms < START_DELAY_MS + set_grid_stage * action_interval_ms + 400
        {
            return None;
        }
        if UI_LAB_NEXT_ACTION
            .compare_exchange(next, next + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }

        Some(if next == 0 {
            super::UiLabAction::SetDetails
        } else if next <= splits {
            super::UiLabAction::SplitHorizontal
        } else if next <= splits * 2 {
            super::UiLabAction::ClosePane
        } else if next == set_grid_stage {
            super::UiLabAction::SetGrid
        } else {
            super::UiLabAction::Quit
        })
    }

    pub(crate) struct Span {
        name: &'static str,
        detail: Option<String>,
        start: Instant,
        #[cfg(feature = "tracy")]
        _tracy: tracy_client::Span,
    }

    impl Drop for Span {
        fn drop(&mut self) {
            let cfg = config();
            let elapsed = self.start.elapsed();
            // Always accumulate (even sub-threshold) so multi-pane sums show up.
            if cfg.enabled {
                let us = elapsed.as_micros() as u64;
                if us > 0 {
                    time_us(self.name, us);
                }
            }
            if elapsed < cfg.threshold {
                return;
            }
            let ms = elapsed.as_secs_f64() * 1000.0;
            match &self.detail {
                Some(detail) => log(format_args!(
                    "[atlas-perf +{:>8.1}ms] {:>7.2}ms {} {}",
                    cfg.started.elapsed().as_secs_f64() * 1000.0,
                    ms,
                    self.name,
                    detail
                )),
                None => log(format_args!(
                    "[atlas-perf +{:>8.1}ms] {:>7.2}ms {}",
                    cfg.started.elapsed().as_secs_f64() * 1000.0,
                    ms,
                    self.name
                )),
            }
        }
    }
}

#[cfg(any(feature = "perf-log", feature = "tracy"))]
pub(crate) use imp::*;

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn next_frame() -> u64 {
    0
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn next_dock_frame() -> u64 {
    0
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn count(_name: &'static str) {}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn count_by(_name: &'static str, _amount: u64) {}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn time_us(_name: &'static str, _us: u64) {}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn frame_summary(_frame: u64, _detail: impl FnOnce() -> String) {}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn span(_name: &'static str) -> Option<Span> {
    None
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn span_detail(_name: &'static str, _detail: impl FnOnce() -> String) -> Option<Span> {
    None
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn event(_detail: impl FnOnce() -> String) {}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn skip_sidebar() -> bool {
    false
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn skip_overlays() -> bool {
    false
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn skip_inspector() -> bool {
    false
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn ui_lab_active() -> bool {
    false
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) fn ui_lab_action() -> Option<UiLabAction> {
    None
}

#[cfg(not(any(feature = "perf-log", feature = "tracy")))]
pub(crate) struct Span;
