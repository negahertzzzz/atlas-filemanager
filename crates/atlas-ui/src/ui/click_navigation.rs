//! A folder click is committed on release, so the same folder can still be dragged.
use atlas_model::{location::Location, pane::TabId};
use gpui::{Pixels, Point};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) struct NavigationClick {
    pub pane: TabId,
    pub origin: Point<Pixels>,
    pub at: Instant,
}
impl NavigationClick {
    pub fn suppresses(
        &self,
        pane: TabId,
        point: Point<Pixels>,
        clicks: usize,
        now: Instant,
        interval: Duration,
    ) -> bool {
        let dx = f32::from(point.x - self.origin.x);
        let dy = f32::from(point.y - self.origin.y);
        self.pane == pane
            && clicks >= 2
            && now.saturating_duration_since(self.at) <= interval
            && dx * dx + dy * dy <= 16.0
    }
}

pub(crate) struct FolderClick {
    pub pane: TabId,
    pub index: usize,
    pub tree: bool,
    pub source: Location,
    pub path: PathBuf,
    pub origin: Point<Pixels>,
}

impl FolderClick {
    pub fn moved(&self, point: Point<Pixels>) -> bool {
        let dx = f32::from(point.x - self.origin.x);
        let dy = f32::from(point.y - self.origin.y);
        dx * dx + dy * dy > 16.0
    }
    pub fn accepts_release(&self, point: Point<Pixels>, source: &Location, marquee: bool) -> bool {
        !self.moved(point) && source == &self.source && !marquee
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_model::layout_tree::TileId;
    #[test]
    fn a_drag_marquee_or_navigation_cancels_the_folder_click() {
        let click = FolderClick {
            pane: TileId(1),
            index: 0,
            tree: false,
            source: Location::Directory("/parent".into()),
            path: "/parent/child".into(),
            origin: gpui::point(gpui::px(10.0), gpui::px(10.0)),
        };
        let point = |x, y| gpui::point(gpui::px(x), gpui::px(y));
        assert!(click.accepts_release(point(11.0, 11.0), &click.source, false));
        assert!(!click.accepts_release(point(20.0, 10.0), &click.source, false));
        assert!(!click.accepts_release(point(10.0, 10.0), &click.source, true));
        assert!(!click.accepts_release(
            point(10.0, 10.0),
            &Location::Directory("/other".into()),
            false
        ));
    }
    #[test]
    fn second_half_of_a_navigation_click_cannot_activate_the_new_listing() {
        let now = Instant::now();
        let pane = TileId(1);
        let origin = gpui::point(gpui::px(10.0), gpui::px(10.0));
        let click = NavigationClick {
            pane,
            origin,
            at: now,
        };
        let interval = Duration::from_millis(500);
        assert!(click.suppresses(pane, origin, 2, now + Duration::from_millis(100), interval));
        assert!(!click.suppresses(pane, origin, 1, now, interval));
        assert!(!click.suppresses(pane, origin, 2, now + Duration::from_secs(1), interval));
        assert!(!click.suppresses(TileId(2), origin, 2, now, interval));
    }
}
