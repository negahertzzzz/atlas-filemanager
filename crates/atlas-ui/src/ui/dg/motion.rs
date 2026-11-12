//! Presentation-only spring. Retargeting preserves both position and velocity.
use atlas_model::clamp::ClampLenient;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

pub(crate) fn presentation_epoch(epoch: u64, viewport: &[f32]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    epoch.hash(&mut hash);
    for value in viewport {
        value.to_bits().hash(&mut hash);
    }
    hash.finish()
}

/// Short action acknowledgement. Repeated activation restarts just this control.
#[derive(Default)]
pub(crate) struct ActionPulse<K> {
    last: Option<(K, Instant)>,
}

impl<K: PartialEq> ActionPulse<K> {
    pub fn trigger(&mut self, key: K, now: Instant) {
        self.last = Some((key, now));
    }

    pub fn sample(&self, key: &K, now: Instant, reduced: bool) -> (f32, bool) {
        let Some((last, at)) = &self.last else {
            return (0.0, false);
        };
        if last != key || reduced {
            return (0.0, false);
        }
        let t = now.saturating_duration_since(*at).as_secs_f32() / 0.14;
        (1.0 - crate::ui::motion::ease_out_quart(t), t < 1.0)
    }
}

/// Reveal the focused Miller column; manual scrolling interrupts the reveal.
pub(crate) struct ColumnReveal {
    spring: RetainedSpring,
    target: f32,
    last_applied: f32,
    focus: Option<(usize, usize, u32, u32)>,
}

impl ColumnReveal {
    pub fn new(offset: f32, now: Instant) -> Self {
        Self {
            spring: RetainedSpring::new(offset, now),
            target: offset,
            last_applied: offset,
            focus: None,
        }
    }

    pub fn sample(
        &mut self,
        focus: usize,
        count: usize,
        col_w: f32,
        viewport_w: f32,
        offset: f32,
        instant: bool,
        now: Instant,
    ) -> (f32, bool) {
        let max = (count as f32 * col_w - viewport_w).max(0.0);
        let manual = (offset - self.last_applied).abs() > 0.5;
        if manual {
            self.target = offset;
            self.spring.sample(offset, true, now);
        }
        let key = (focus, count, viewport_w.to_bits(), col_w.to_bits());
        if self.focus != Some(key) {
            // Clicking a directory usually leaves keyboard focus on its parent
            // column. Reveal the newly appended child as well.
            let revealed = if self
                .focus
                .is_some_and(|(_, previous_count, _, _)| count > previous_count)
            {
                count.saturating_sub(1)
            } else {
                focus
            };
            let left = revealed as f32 * col_w;
            let right = left + col_w;
            self.target = if right > offset + viewport_w {
                right - viewport_w
            } else if left < offset {
                left
            } else {
                offset
            };
            self.focus = Some(key);
        }
        self.target = self.target.clamp_lenient(0.0, max);
        let (value, active) = self.spring.sample(self.target, instant, now);
        self.last_applied = value.clamp_lenient(0.0, max);
        (self.last_applied, active)
    }
}

pub(crate) struct RetainedSpring {
    state: gpui::SpringState,
    target: f32,
    sampled_at: Instant,
}

/// Fade only newly delivered resources. Cached images and scrolling stay direct;
/// this state stores IDs, never the image buffers, and prunes offscreen entries.
pub(crate) struct ResourceFade<K> {
    items: HashMap<K, (Option<gpui::ImageId>, Option<Instant>)>,
}

impl<K: Eq + Hash + Clone> ResourceFade<K> {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
        }
    }
    pub fn sample(
        &mut self,
        values: &[(K, Option<gpui::ImageId>)],
        instant: bool,
        now: Instant,
    ) -> (Vec<f32>, bool) {
        let mut next = HashMap::with_capacity(values.len());
        let mut active = false;
        let samples = values
            .iter()
            .map(|(key, id)| {
                let start = match self.items.get(key) {
                    Some((before, at)) if before == id => *at,
                    Some(_) if id.is_some() && !instant => Some(now),
                    _ => None,
                };
                let t = start.map_or(1.0, |at| {
                    now.saturating_duration_since(at).as_secs_f32() / 0.14
                });
                let running = !instant && t < 1.0;
                active |= running;
                next.insert(key.clone(), (*id, if running { start } else { None }));
                if running {
                    0.3 + 0.7 * crate::ui::motion::ease_out_quart(t)
                } else {
                    1.0
                }
            })
            .collect();
        self.items = next;
        (samples, active)
    }
}

/// Two-axis FLIP for visible cells; each coordinate uses the same retained solver.
pub(crate) struct CellMotion<K> {
    x: RowMotion<K>,
    y: RowMotion<K>,
}

impl<K: Eq + Hash + Clone> CellMotion<K> {
    pub fn new(epoch: u64, viewport: [f32; 3]) -> Self {
        Self {
            x: RowMotion::new(epoch, viewport),
            y: RowMotion::new(epoch, viewport),
        }
    }

    pub fn sample(
        &mut self,
        targets: &[(K, usize, f32, f32)],
        epoch: u64,
        viewport: [f32; 3],
        instant: bool,
        now: Instant,
    ) -> (Vec<(usize, f32, f32)>, bool) {
        let xs: Vec<_> = targets
            .iter()
            .map(|(key, i, x, _)| (key.clone(), *i, *x))
            .collect();
        let ys: Vec<_> = targets
            .iter()
            .map(|(key, i, _, y)| (key.clone(), *i, *y))
            .collect();
        let (xs, ax) = self.x.sample(&xs, epoch, viewport, instant, now);
        let (ys, ay) = self.y.sample(&ys, epoch, viewport, instant, now);
        (
            xs.into_iter()
                .zip(ys)
                .map(|((i, x, _), (_, y, _))| (i, x, y))
                .collect(),
            ax || ay,
        )
    }
}

/// 2D FLIP bounds for a grid cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GridItemBounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug)]
struct GridMotionItem {
    visual: GridItemBounds,
    target: GridItemBounds,
    start_delta: GridItemBounds,
    start_time: Instant,
    animating: bool,
    seen_frame: u64,
}

/// 2D FLIP motion for grid cells tracking both (x, y) position and (w, h) dimensions.
/// Animates smoothly during zoom (Ctrl+wheel / size slider), column reflow, window resize,
/// and pane splits/spawns without snapping or jumping.
pub(crate) struct GridMotion<K> {
    items: HashMap<K, GridMotionItem>,
    epoch: u64,
    frame: u64,
}

impl<K: Eq + Hash + Clone> GridMotion<K> {
    pub const DURATION: Duration = Duration::from_millis(180);

    pub fn new(epoch: u64) -> Self {
        Self {
            items: HashMap::new(),
            epoch,
            frame: 0,
        }
    }

    pub fn sample(
        &mut self,
        targets: &[(K, usize, GridItemBounds)],
        epoch: u64,
        instant: bool,
        now: Instant,
    ) -> (Vec<(usize, GridItemBounds)>, bool) {
        let mut identities = std::collections::HashSet::with_capacity(targets.len());
        if targets.iter().any(|(key, _, _)| !identities.insert(key)) {
            self.items.clear();
            self.epoch = epoch;
            return (
                targets
                    .iter()
                    .map(|(_, index, bounds)| (*index, *bounds))
                    .collect(),
                false,
            );
        }

        self.frame = self.frame.wrapping_add(1);
        let current_frame = self.frame;
        let mut active = false;
        let mut results = Vec::with_capacity(targets.len());

        for (key, index, target) in targets {
            let item = match self.items.get_mut(key) {
                Some(existing) => {
                    // A cell that was not in the previous frame left the realised
                    // span (scrolled away). Its retained bounds predate any zoom or
                    // reflow since, so re-entering must snap, never replay a FLIP
                    // from that stale geometry.
                    let stale = existing.seen_frame.wrapping_add(1) != current_frame;
                    existing.seen_frame = current_frame;
                    if instant || stale {
                        existing.visual = *target;
                        existing.target = *target;
                        existing.animating = false;
                    } else {
                        let dx = existing.visual.x - target.x;
                        let dy = existing.visual.y - target.y;
                        let dw = existing.visual.w - target.w;
                        let dh = existing.visual.h - target.h;
                        let target_changed = (existing.target.x - target.x).abs() > 0.5
                            || (existing.target.y - target.y).abs() > 0.5
                            || (existing.target.w - target.w).abs() > 0.5
                            || (existing.target.h - target.h).abs() > 0.5;

                        if target_changed
                            && (dx.abs() > 0.5
                                || dy.abs() > 0.5
                                || dw.abs() > 0.5
                                || dh.abs() > 0.5)
                        {
                            existing.start_delta = GridItemBounds {
                                x: dx,
                                y: dy,
                                w: dw,
                                h: dh,
                            };
                            existing.target = *target;
                            existing.start_time = now;
                            existing.animating = true;
                            active = true;
                        } else if existing.animating {
                            let elapsed = now.saturating_duration_since(existing.start_time);
                            if elapsed < Self::DURATION {
                                let t = (elapsed.as_secs_f32() / Self::DURATION.as_secs_f32())
                                    .clamp(0.0, 1.0);
                                let factor = 1.0 - crate::ui::motion::ease_out_quart(t);
                                existing.visual = GridItemBounds {
                                    x: existing.target.x + existing.start_delta.x * factor,
                                    y: existing.target.y + existing.start_delta.y * factor,
                                    w: (existing.target.w + existing.start_delta.w * factor)
                                        .max(1.0),
                                    h: (existing.target.h + existing.start_delta.h * factor)
                                        .max(1.0),
                                };
                                existing.animating = true;
                                active = true;
                            } else {
                                existing.visual = existing.target;
                                existing.animating = false;
                            }
                        } else {
                            existing.visual = *target;
                            existing.target = *target;
                        }
                    }
                    existing.visual
                }
                None => {
                    self.items.insert(
                        key.clone(),
                        GridMotionItem {
                            visual: *target,
                            target: *target,
                            start_delta: GridItemBounds {
                                x: 0.0,
                                y: 0.0,
                                w: 0.0,
                                h: 0.0,
                            },
                            start_time: now,
                            animating: false,
                            seen_frame: current_frame,
                        },
                    );
                    *target
                }
            };

            results.push((*index, item));
        }

        if self.frame % 30 == 0 {
            self.items.retain(|_, item| {
                item.animating || current_frame.saturating_sub(item.seen_frame) < 120
            });
        }

        self.epoch = epoch;
        (results, active)
    }
}

/// Retains only a viewport's presentation snapshots while they enter or leave.
/// Departing snapshots are paint-only: callers must never register their input.
pub(crate) struct PresenceMotion<K, V> {
    items: HashMap<K, PresenceItem<V>>,
    epoch: u64,
}

struct PresenceItem<V> {
    value: V,
    opacity: RetainedSpring,
}

pub(crate) struct PresenceSample<V> {
    pub value: V,
    pub opacity: f32,
    pub interactive: bool,
}

impl<K: Eq + Hash + Clone, V: Clone> PresenceMotion<K, V> {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
            epoch: 0,
        }
    }

    pub fn sample(
        &mut self,
        values: Vec<(K, V)>,
        epoch: u64,
        instant: bool,
        now: Instant,
    ) -> (Vec<PresenceSample<V>>, bool) {
        let mut identities = std::collections::HashSet::with_capacity(values.len());
        if values.iter().any(|(key, _)| !identities.insert(key)) {
            self.items.clear();
            self.epoch = epoch;
            return (
                values
                    .into_iter()
                    .map(|(_, value)| PresenceSample {
                        value,
                        opacity: 1.0,
                        interactive: true,
                    })
                    .collect(),
                false,
            );
        }
        let reset = instant || epoch != self.epoch;
        if reset {
            self.items.clear();
        }
        let reveal = !self.items.is_empty();
        let keys: std::collections::HashSet<_> =
            values.iter().map(|(key, _)| key.clone()).collect();
        let mut samples = Vec::with_capacity(self.items.len().max(values.len()));
        let mut active = false;
        // Exits are below current content and cannot steal clicks or drops.
        self.items.retain(|key, item| {
            if keys.contains(key) {
                return true;
            }
            let (opacity, running) = item.opacity.sample_unit(0.0, reset, now);
            active |= running;
            if running {
                samples.push(PresenceSample {
                    value: item.value.clone(),
                    opacity: opacity.clamp(0.0, 1.0),
                    interactive: false,
                });
            }
            running
        });
        for (key, value) in values {
            let item = self.items.entry(key).or_insert_with(|| PresenceItem {
                value: value.clone(),
                opacity: RetainedSpring::new(if reveal && !reset { 0.0 } else { 1.0 }, now),
            });
            item.value = value.clone();
            let (opacity, running) = item.opacity.sample_unit(1.0, reset, now);
            active |= running;
            samples.push(PresenceSample {
                value,
                opacity: opacity.clamp(0.0, 1.0),
                interactive: true,
            });
        }
        self.epoch = epoch;
        (samples, active)
    }
}

/// Bounded boolean transitions for disclosure chevrons, keyed by directory identity.
pub(crate) struct DisclosureMotion<K> {
    items: HashMap<K, RetainedSpring>,
}

impl<K: Eq + Hash + Clone> DisclosureMotion<K> {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
        }
    }

    pub fn sample(
        &mut self,
        values: &[(K, bool)],
        instant: bool,
        now: Instant,
    ) -> (Vec<f32>, bool) {
        let mut next = HashMap::with_capacity(values.len());
        let mut active = false;
        let samples = values
            .iter()
            .map(|(key, open)| {
                let goal = if *open { 1.0 } else { 0.0 };
                let mut spring = self
                    .items
                    .remove(key)
                    .unwrap_or_else(|| RetainedSpring::new(goal, now));
                let (value, running) = spring.sample_unit(goal, instant, now);
                active |= running;
                next.insert(key.clone(), spring);
                value.clamp(0.0, 1.0)
            })
            .collect();
        self.items = next;
        (samples, active)
    }
}

/// FLIP positions for a bounded visible set, keyed by immutable item identity.
/// Rows entering through scrolling start at their destination; only survivors move.
pub(crate) struct RowMotion<K> {
    rows: HashMap<K, RowSpring>,
    epoch: u64,
    viewport: [f32; 3],
    frame: u64,
    /// Survivors travelling further than this many rows land near their slot
    /// instead of sweeping across unrelated rows (sorting). Disclosure raises it
    /// to the viewport so pushed neighbours always slide.
    pub long_journey_rows: f32,
}

struct RowSpring {
    spring: RetainedSpring,
    seen: u64,
    slot: usize,
    reveal_at: Option<Instant>,
}

impl<K: Eq + Hash + Clone> RowMotion<K> {
    pub fn new(epoch: u64, viewport: [f32; 3]) -> Self {
        Self {
            rows: HashMap::new(),
            epoch,
            viewport,
            frame: 0,
            long_journey_rows: 3.0,
        }
    }

    pub fn sample(
        &mut self,
        targets: &[(K, usize, f32)],
        epoch: u64,
        viewport: [f32; 3],
        instant: bool,
        now: Instant,
    ) -> (Vec<(usize, f32, f32)>, bool) {
        // Providers such as Drive allow duplicate names/paths. Those are not
        // unique FLIP identities: sharing one spring makes the rows fight over
        // its target forever (and overlap until dragging forces instant layout).
        // Keep every row at its own slot until the caller has unambiguous IDs.
        let mut identities = std::collections::HashSet::with_capacity(targets.len());
        if targets.iter().any(|(key, _, _)| !identities.insert(key)) {
            self.rows.clear();
            self.epoch = epoch;
            self.viewport = viewport;
            return (
                targets
                    .iter()
                    .map(|(_, index, y)| (*index, *y, 1.0))
                    .collect(),
                false,
            );
        }
        let scroll_changed = (viewport[0] - self.viewport[0]).abs() > 1.0;
        let size_changed = (viewport[1] - self.viewport[1]).abs() > 0.01
            || (viewport[2] - self.viewport[2]).abs() > 0.01;
        let instant = instant || epoch != self.epoch || scroll_changed || size_changed;
        let changed = !self.rows.is_empty()
            && (self.rows.len() != targets.len()
                || targets
                    .iter()
                    .any(|(key, index, _)| self.rows.get(key).is_none_or(|r| r.slot != *index)));
        self.frame = self.frame.wrapping_add(1);
        let mut positions = Vec::with_capacity(targets.len());
        let mut active = false;
        for (key, index, y) in targets {
            let reveal_at = now;
            if !self.rows.contains_key(key) {
                let reveal = changed && !instant;
                let mut spring =
                    RetainedSpring::new(*y + if reveal { viewport[1] * 0.15 } else { 0.0 }, now);
                if reveal {
                    spring.target = *y;
                    spring.sampled_at = reveal_at;
                }
                self.rows.insert(
                    key.clone(),
                    RowSpring {
                        spring,
                        seen: self.frame,
                        slot: *index,
                        reveal_at: reveal.then_some(reveal_at),
                    },
                );
            }
            let row = self.rows.get_mut(key).expect("row just inserted");
            row.seen = self.frame;
            // Long journeys would sweep across dozens of unrelated files. Land
            // near the destination and reveal; nearby survivors keep true FLIP.
            if changed
                && !instant
                && row.slot != *index
                && (row.spring.state.position - y).abs() > viewport[1] * self.long_journey_rows
            {
                let direction = (row.spring.state.position - y).signum();
                row.spring = RetainedSpring::new(*y + direction * viewport[1] * 0.15, now);
                row.spring.target = *y;
                row.spring.sampled_at = reveal_at;
                row.reveal_at = Some(reveal_at);
            }
            row.slot = *index;
            if instant {
                row.reveal_at = None;
            }
            let opacity = row.reveal_at.map_or(1.0, |start| {
                0.9 + 0.1
                    * crate::ui::motion::ease_out_quart(
                        now.saturating_duration_since(start).as_secs_f32() * 1000.0
                            / crate::ui::motion::ROW_REORDER_FADE_MS,
                    )
            });
            let (value, running) = if row.reveal_at.is_some_and(|start| now < start) {
                (row.spring.state.position, true)
            } else {
                row.spring.sample_with_config(
                    *y,
                    instant,
                    now,
                    crate::ui::motion::ROW_REORDER_SPRING.config(),
                )
            };
            if opacity >= 1.0 {
                row.reveal_at = None;
            }
            positions.push((*index, value, opacity));
            active |= running || opacity < 1.0;
        }
        self.rows.retain(|_, row| row.seen == self.frame);
        self.epoch = epoch;
        self.viewport = viewport;
        (positions, active)
    }
}

impl RetainedSpring {
    pub fn new(value: f32, now: Instant) -> Self {
        Self {
            state: gpui::SpringState {
                position: value,
                velocity: 0.0,
            },
            target: value,
            sampled_at: now,
        }
    }

    pub fn sample(&mut self, target: f32, instant: bool, now: Instant) -> (f32, bool) {
        self.sample_with_config(
            target,
            instant,
            now,
            crate::ui::motion::TABLE_SPRING.config(),
        )
    }

    pub(crate) fn sample_with_config(
        &mut self,
        target: f32,
        instant: bool,
        now: Instant,
        config: gpui::SpringConfig,
    ) -> (f32, bool) {
        self.sample_with_precision(target, instant, now, config, 0.1)
    }

    /// Normalized opacity/rotation needs a tighter tolerance than pixel geometry.
    pub(crate) fn sample_unit(&mut self, target: f32, instant: bool, now: Instant) -> (f32, bool) {
        self.sample_with_precision(
            target,
            instant,
            now,
            crate::ui::motion::TABLE_SPRING.config(),
            0.001,
        )
    }

    fn sample_with_precision(
        &mut self,
        target: f32,
        instant: bool,
        now: Instant,
        config: gpui::SpringConfig,
        epsilon: f32,
    ) -> (f32, bool) {
        // Advance the old goal before changing it. There is exactly one step per sample.
        let elapsed = now.saturating_duration_since(self.sampled_at).as_secs_f32();
        self.state = config.step(
            self.state,
            self.target,
            if elapsed > 0.25 { 0.0 } else { elapsed },
        );
        self.sampled_at = now;
        self.target = target;
        if instant || config.is_settled(self.state, target, epsilon) {
            self.state = gpui::SpringState {
                position: target,
                velocity: 0.0,
            };
            (target, false)
        } else {
            (self.state.position, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn duplicate_provider_paths_never_share_an_animated_row_position() {
        let now = Instant::now();
        let viewport = [0.0, 26.0, 800.0];
        let mut motion = RowMotion::new(1, viewport);
        motion.sample(&[("other", 0, 0.0)], 1, viewport, false, now);
        let targets = [
            ("duplicate", 0, 0.0),
            ("duplicate", 1, 26.0),
            ("other", 2, 52.0),
        ];
        for frame in 0..120 {
            let (positions, active) = motion.sample(
                &targets,
                1,
                viewport,
                frame == 60,
                now + Duration::from_millis(frame * 16),
            );
            assert_eq!(positions, [(0, 0.0, 1.0), (1, 26.0, 1.0), (2, 52.0, 1.0)]);
            assert!(
                !active,
                "duplicate names must not drive an endless animation"
            );
        }
    }

    #[test]
    fn duplicate_presence_keys_keep_every_row_visible_and_interactive() {
        let now = Instant::now();
        let mut presence = PresenceMotion::new();
        presence.sample(vec![("existing", 0)], 1, false, now);
        let (samples, active) =
            presence.sample(vec![("duplicate", 1), ("duplicate", 2)], 1, false, now);
        assert!(!active);
        assert_eq!(samples.len(), 2);
        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.value)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(samples
            .iter()
            .all(|sample| sample.interactive && sample.opacity == 1.0));
    }

    #[test]
    fn suspended_spring_resumes_without_consuming_idle_time() {
        let now = Instant::now();
        let mut spring = RetainedSpring::new(0.0, now);
        spring.sample(100.0, false, now);
        let resumed = now + Duration::from_secs(10);
        let (position, active) = spring.sample(100.0, false, resumed);
        assert_eq!(position, 0.0);
        assert!(active);
        let (position, _) = spring.sample(100.0, false, resumed + Duration::from_millis(16));
        assert!(position > 0.0 && position < 100.0);
    }

    #[test]
    fn grid_cells_reentering_after_a_zoom_snap_instead_of_animating() {
        let now = Instant::now();
        let small = GridItemBounds {
            x: 0.0,
            y: 0.0,
            w: 80.0,
            h: 100.0,
        };
        let big = GridItemBounds {
            x: 0.0,
            y: 400.0,
            w: 160.0,
            h: 200.0,
        };
        let mut motion = GridMotion::new(1);
        motion.sample(&[("a", 0, small), ("b", 1, small)], 1, false, now);
        // Scrolled away: only "b" is realised while the zoom changes.
        motion.sample(&[("b", 1, big)], 1, false, now);
        // "a" scrolls back into view at its new geometry: no replay from `small`.
        let (cells, _) = motion.sample(&[("a", 0, big), ("b", 1, big)], 1, false, now);
        assert_eq!(cells[0].1, big);
    }

    #[test]
    fn cell_reflow_keeps_both_coordinates_and_resets_for_scroll_or_motion_disable() {
        let now = Instant::now();
        let viewport = [0.0, 100.0, 500.0];
        let mut motion = CellMotion::new(1, viewport);
        motion.sample(
            &[("a", 0, 0.0, 0.0), ("b", 1, 120.0, 0.0)],
            1,
            viewport,
            false,
            now,
        );
        let next = [("b", 0, 0.0, 0.0), ("a", 2, 0.0, 120.0)];
        let (cells, active) = motion.sample(&next, 1, viewport, false, now);
        assert!(active);
        assert_eq!(cells, [(0, 120.0, 0.0), (2, 0.0, 0.0)]);
        for frame in 1..125 {
            motion.sample(
                &next,
                1,
                viewport,
                false,
                now + Duration::from_millis(frame * 16),
            );
        }
        let (cells, active) =
            motion.sample(&next, 1, viewport, false, now + Duration::from_secs(2));
        assert!(!active);
        assert_eq!(cells, [(0, 0.0, 0.0), (2, 0.0, 120.0)]);
        let (cells, active) =
            motion.sample(&[("a", 0, 10.0, 20.0)], 1, [30.0, 100.0, 500.0], false, now);
        assert_eq!(cells, [(0, 10.0, 20.0)]);
        assert!(!active);
        assert!(
            !motion
                .sample(&[("a", 1, 200.0, 40.0)], 1, viewport, true, now)
                .1
        );
    }

    #[test]
    fn column_reveal_retargets_and_manual_scroll_interrupts_without_fighting_the_pointer() {
        let now = Instant::now();
        let mut reveal = ColumnReveal::new(0.0, now);
        assert_eq!(
            reveal.sample(0, 1, 250.0, 500.0, 0.0, false, now),
            (0.0, false)
        );
        assert_eq!(
            reveal.sample(2, 3, 250.0, 500.0, 0.0, false, now),
            (0.0, true)
        );
        let (offset, active) = reveal.sample(
            2,
            3,
            250.0,
            500.0,
            0.0,
            false,
            now + Duration::from_millis(40),
        );
        assert!(active && offset > 0.0 && offset < 250.0);
        assert_eq!(
            reveal.sample(
                2,
                3,
                250.0,
                500.0,
                30.0,
                false,
                now + Duration::from_millis(40)
            ),
            (30.0, false)
        );
        assert_eq!(
            reveal.sample(2, 3, 250.0, 300.0, 30.0, true, now),
            (450.0, false)
        );
        assert_eq!(
            reveal.sample(0, 1, 250.0, 500.0, 450.0, true, now),
            (0.0, false)
        );
    }

    #[test]
    fn appended_child_column_is_revealed_even_when_focus_stays_on_its_parent() {
        let now = Instant::now();
        let mut reveal = ColumnReveal::new(0.0, now);
        reveal.sample(0, 1, 250.0, 400.0, 0.0, false, now);
        assert_eq!(
            reveal.sample(0, 2, 250.0, 400.0, 0.0, true, now),
            (100.0, false)
        );
    }

    #[test]
    fn resource_delivery_fades_once_cached_images_do_not_flash_and_offscreen_ids_are_pruned() {
        let now = Instant::now();
        let id = gpui::ImageId(123);
        let mut fade = ResourceFade::new();
        assert_eq!(fade.sample(&[("a", None)], false, now), (vec![1.0], false));
        assert_eq!(
            fade.sample(&[("a", Some(id))], false, now),
            (vec![0.3], true)
        );
        let (alpha, active) =
            fade.sample(&[("a", Some(id))], false, now + Duration::from_millis(50));
        assert!(active && alpha[0] > 0.3 && alpha[0] < 1.0);
        assert_eq!(
            fade.sample(&[("a", Some(id))], false, now + Duration::from_secs(1)),
            (vec![1.0], false)
        );
        fade.sample(&[], false, now);
        assert!(fade.items.is_empty());
        assert_eq!(
            fade.sample(&[("a", Some(id))], false, now),
            (vec![1.0], false)
        );
        assert_eq!(
            fade.sample(&[("a", Some(gpui::ImageId(124)))], true, now),
            (vec![1.0], false)
        );
    }

    #[test]
    fn control_feedback_repeated_activation_only_restarts_the_used_control() {
        let now = Instant::now();
        let mut pulse = ActionPulse::<u8>::default();
        pulse.trigger(1, now);
        assert_eq!(pulse.sample(&1, now, false), (1.0, true));
        assert_eq!(pulse.sample(&2, now, false), (0.0, false));
        let later = now + Duration::from_millis(80);
        assert!(pulse.sample(&1, later, false).0 < 1.0);
        pulse.trigger(2, later);
        assert_eq!(pulse.sample(&1, later, false), (0.0, false));
        assert_eq!(pulse.sample(&2, later, false), (1.0, true));
        assert_eq!(pulse.sample(&2, later, true), (0.0, false));
        assert_eq!(
            pulse.sample(&2, later + Duration::from_secs(1), false),
            (0.0, false)
        );
    }

    #[test]
    fn presence_exits_are_paint_only_and_settle_without_extra_frames() {
        let now = Instant::now();
        let mut motion = PresenceMotion::new();
        let (initial, active) = motion.sample(vec![("a", 10), ("b", 20)], 1, false, now);
        assert!(!active);
        assert!(initial
            .iter()
            .all(|item| item.interactive && item.opacity == 1.0));
        let (leaving, active) = motion.sample(vec![("a", 11)], 1, false, now);
        assert!(active);
        assert_eq!(leaving[0].value, 20);
        assert!(!leaving[0].interactive);
        assert!(leaving[1].interactive);
        assert_eq!(leaving[1].value, 11);
        for frame in 1..125 {
            motion.sample(
                vec![("a", 11)],
                1,
                false,
                now + Duration::from_millis(frame * 16),
            );
        }
        let (settled, active) =
            motion.sample(vec![("a", 11)], 1, false, now + Duration::from_secs(2));
        assert!(!active);
        assert_eq!(settled.len(), 1);
        assert_eq!(motion.items.len(), 1);
    }

    #[test]
    fn presence_reversal_preserves_opacity_and_restores_input_once() {
        let now = Instant::now();
        let mut motion = PresenceMotion::new();
        motion.sample(vec![("a", 10)], 1, false, now);
        motion.sample(vec![], 1, false, now);
        let time = now + Duration::from_millis(40);
        let (leaving, _) = motion.sample(vec![], 1, false, time);
        assert!(leaving[0].opacity > 0.0 && leaving[0].opacity < 1.0);
        let (returning, active) = motion.sample(vec![("a", 12)], 1, false, time);
        assert!(active);
        assert_eq!(returning.len(), 1);
        assert_eq!(returning[0].opacity, leaving[0].opacity);
        assert!(returning[0].interactive);
        assert_eq!(returning[0].value, 12);
    }

    #[test]
    fn presence_navigation_and_motion_disable_discard_stale_snapshots() {
        let now = Instant::now();
        for (epoch, instant) in [(2, false), (1, true)] {
            let mut motion = PresenceMotion::new();
            motion.sample(vec![("a", 10), ("b", 20)], 1, false, now);
            let (samples, active) = motion.sample(vec![("c", 30)], epoch, instant, now);
            assert!(!active);
            assert_eq!(samples.len(), 1);
            assert_eq!(samples[0].value, 30);
            assert_eq!(samples[0].opacity, 1.0);
            assert_eq!(motion.items.len(), 1);
        }
    }

    #[test]
    fn presence_new_content_reveals_without_restarting_survivors() {
        let now = Instant::now();
        let mut motion = PresenceMotion::new();
        motion.sample(vec![("a", 10)], 1, false, now);
        let values = vec![("a", 10), ("b", 20)];
        let (samples, active) = motion.sample(values.clone(), 1, false, now);
        assert!(active);
        assert_eq!(samples[0].opacity, 1.0);
        assert_eq!(samples[1].opacity, 0.0);
        let (samples, active) =
            motion.sample(values.clone(), 1, false, now + Duration::from_millis(50));
        assert!(active);
        assert_eq!(samples[0].opacity, 1.0);
        assert!(samples[1].opacity > 0.0 && samples[1].opacity < 1.0);
        for frame in 4..125 {
            motion.sample(
                values.clone(),
                1,
                false,
                now + Duration::from_millis(frame * 16),
            );
        }
        let (_, active) = motion.sample(values, 1, false, now + Duration::from_secs(2));
        assert!(!active);
    }

    #[test]
    fn disclosure_can_reverse_and_discards_offscreen_branches() {
        let now = Instant::now();
        let mut motion = DisclosureMotion::new();
        assert_eq!(
            motion.sample(&[("a", false), ("b", true)], false, now),
            (vec![0.0, 1.0], false)
        );
        motion.sample(&[("a", true)], false, now);
        let time = now + Duration::from_millis(40);
        let (open, _) = motion.sample(&[("a", true)], false, time);
        let (close, active) = motion.sample(&[("a", false)], false, time);
        assert_eq!(open, close);
        assert!(active);
        assert_eq!(motion.items.len(), 1);
        assert_eq!(
            motion.sample(&[("a", false)], true, time),
            (vec![0.0], false)
        );
        assert_eq!(motion.sample(&[], false, time), (vec![], false));
        assert!(motion.items.is_empty());
    }

    #[test]
    fn reversal_keeps_position_and_velocity_then_disable_settles() {
        let now = Instant::now();
        let mut spring = RetainedSpring::new(0.0, now);
        spring.sample(100.0, false, now);
        let time = now + Duration::from_millis(50);
        let (before, _) = spring.sample(100.0, false, time);
        let velocity = spring.state.velocity;
        let (after, active) = spring.sample(0.0, false, time);
        assert_eq!(before, after);
        assert_eq!(spring.state.velocity, velocity);
        assert!(active);
        assert_eq!(spring.sample(0.0, true, time), (0.0, false));
        assert_eq!(
            spring.sample(0.0, false, time + Duration::from_secs(1)),
            (0.0, false)
        );
    }

    #[test]
    fn samples_are_independent_of_refresh_rate() {
        let now = Instant::now();
        let mut samples = Vec::new();
        for hz in [30, 60, 144] {
            let mut spring = RetainedSpring::new(0.0, now);
            spring.sample(200.0, false, now);
            for i in 1..=hz / 3 {
                spring.sample(
                    200.0,
                    false,
                    now + Duration::from_secs_f32(i as f32 / hz as f32),
                );
            }
            samples.push(
                spring
                    .sample(200.0, false, now + Duration::from_secs_f32(1.0 / 3.0))
                    .0,
            );
        }
        assert!((samples[0] - samples[1]).abs() < 0.01);
        assert!((samples[0] - samples[2]).abs() < 0.01);
    }

    #[test]
    fn row_identity_survives_sort_but_scroll_and_navigation_are_direct() {
        let now = Instant::now();
        let viewport = [0.0, 24.0, 400.0];
        let mut rows = RowMotion::new(1, viewport);
        rows.sample(&[("a", 0, 0.0), ("b", 1, 24.0)], 1, viewport, false, now);
        let swapped = [("b", 0, 0.0), ("a", 1, 24.0)];
        let (positions, active) = rows.sample(&swapped, 1, viewport, false, now);
        assert!(active);
        assert_eq!(positions, [(0, 24.0, 1.0), (1, 0.0, 1.0)]);
        let (positions_subpixel, active_subpixel) =
            rows.sample(&swapped, 1, [0.25, 24.0, 400.0], false, now);
        assert!(active_subpixel);
        assert_eq!(positions_subpixel, [(0, 24.0, 1.0), (1, 0.0, 1.0)]);
        let (positions, active) = rows.sample(&swapped, 1, [50.0, 24.0, 400.0], false, now);
        assert!(!active);
        assert_eq!(positions, [(0, 0.0, 1.0), (1, 24.0, 1.0)]);
        assert!(!rows.sample(&swapped, 2, viewport, false, now).1);
        assert_eq!(rows.rows.len(), 2);
    }

    #[test]
    fn long_sort_journeys_and_new_visible_files_reveal_near_their_destination() {
        let now = Instant::now();
        let viewport = [0.0, 24.0, 400.0];
        let mut rows = RowMotion::new(1, viewport);
        rows.sample(&[("a", 0, 0.0), ("b", 10, 240.0)], 1, viewport, false, now);
        let sorted = [("b", 0, 0.0), ("new", 1, 24.0), ("a", 10, 240.0)];
        let (positions, active) = rows.sample(&sorted, 1, viewport, false, now);
        assert!(active);
        for ((_, _, destination), (_, y, opacity)) in sorted.iter().zip(&positions) {
            assert!((y - destination).abs() < 24.0);
            assert_eq!(*opacity, 0.9);
        }
        for frame in 1..125 {
            rows.sample(
                &sorted,
                1,
                viewport,
                false,
                now + Duration::from_millis(frame * 16),
            );
        }
        let (positions, active) =
            rows.sample(&sorted, 1, viewport, false, now + Duration::from_secs(2));
        assert!(!active);
        assert_eq!(positions, [(0, 0.0, 1.0), (1, 24.0, 1.0), (10, 240.0, 1.0)]);
    }

    #[test]
    fn attached_column_releases_from_the_pointer_without_a_ghost_jump() {
        let now = Instant::now();
        let mut column = RetainedSpring::new(100.0, now);
        column.sample(380.0, true, now + Duration::from_millis(50));
        assert_eq!(
            column.sample(260.0, false, now + Duration::from_millis(50)),
            (380.0, true)
        );
        let (x, _) = column.sample(260.0, false, now + Duration::from_millis(100));
        assert!(x > 260.0 && x < 380.0);
    }

    #[test]
    fn grid_motion_animates_both_position_and_size_and_handles_retargeting() {
        let now = Instant::now();
        let mut motion = GridMotion::new(1);
        let initial = [(
            "item1",
            0,
            GridItemBounds {
                x: 0.0,
                y: 0.0,
                w: 120.0,
                h: 120.0,
            },
        )];
        let (res, active) = motion.sample(&initial, 1, false, now);
        assert!(!active);
        assert_eq!(res[0].1.x, 0.0);
        assert_eq!(res[0].1.w, 120.0);

        // Zoom: change both pos and dimensions
        let zoomed = [(
            "item1",
            0,
            GridItemBounds {
                x: 20.0,
                y: 30.0,
                w: 180.0,
                h: 180.0,
            },
        )];
        let (res, active) = motion.sample(&zoomed, 1, false, now);
        assert!(active);
        // At start of animation, visual matches previous
        assert_eq!(res[0].1.x, 0.0);
        assert_eq!(res[0].1.y, 0.0);
        assert_eq!(res[0].1.w, 120.0);
        assert_eq!(res[0].1.h, 120.0);

        // Halfway
        let midway = now + Duration::from_millis(90);
        let (res, active) = motion.sample(&zoomed, 1, false, midway);
        assert!(active);
        assert!(res[0].1.x > 0.0 && res[0].1.x < 20.0);
        assert!(res[0].1.y > 0.0 && res[0].1.y < 30.0);
        assert!(res[0].1.w > 120.0 && res[0].1.w < 180.0);
        assert!(res[0].1.h > 120.0 && res[0].1.h < 180.0);

        // Retargeting during flight (e.g. continuous resize or wheel zoom)
        let retargeted = [(
            "item1",
            0,
            GridItemBounds {
                x: 50.0,
                y: 60.0,
                w: 200.0,
                h: 200.0,
            },
        )];
        let (res, active) = motion.sample(&retargeted, 1, false, midway);
        assert!(active);
        // Must start from current in-flight position, not snap
        assert!(res[0].1.x > 0.0 && res[0].1.x < 20.0);

        // Settles after duration
        let settled_time = midway + Duration::from_millis(300);
        let (res, active) = motion.sample(&retargeted, 1, false, settled_time);
        assert!(!active);
        assert_eq!(res[0].1.x, 50.0);
        assert_eq!(res[0].1.y, 60.0);
        assert_eq!(res[0].1.w, 200.0);
        assert_eq!(res[0].1.h, 200.0);

        // Instant snaps immediately
        let instant_target = [(
            "item1",
            0,
            GridItemBounds {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
        )];
        let (res, active) = motion.sample(&instant_target, 1, true, settled_time);
        assert!(!active);
        assert_eq!(res[0].1.x, 0.0);
        assert_eq!(res[0].1.w, 100.0);
    }
}
