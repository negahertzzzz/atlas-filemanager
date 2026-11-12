//! Digital Grove Rectangle Layout (Principle 4).
//!
//! Provides deterministic O(1) single-cycle arithmetic layout by slicing
//! rectangles instead of multi-pass CSS flexbox equation solving.

use crate::ui::surface::Slot;
use gpui::{px, Bounds, Pixels};

#[derive(Clone, Copy, Debug)]
pub struct RectCut {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectCut {
    #[inline]
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub fn from_bounds(bounds: Bounds<Pixels>) -> Self {
        Self {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            w: f32::from(bounds.size.width),
            h: f32::from(bounds.size.height),
        }
    }

    #[inline]
    pub fn cut_left(&mut self, amount: f32) -> Slot {
        let cut_w = amount.min(self.w.max(0.0));
        let res = Slot::new(self.x, self.y, cut_w, self.h);
        self.x += cut_w;
        self.w = (self.w - cut_w).max(0.0);
        res
    }

    #[inline]
    pub fn cut_right(&mut self, amount: f32) -> Slot {
        let cut_w = amount.min(self.w.max(0.0));
        self.w = (self.w - cut_w).max(0.0);
        Slot::new(self.x + self.w, self.y, cut_w, self.h)
    }

    #[inline]
    pub fn cut_top(&mut self, amount: f32) -> Slot {
        let cut_h = amount.min(self.h.max(0.0));
        let res = Slot::new(self.x, self.y, self.w, cut_h);
        self.y += cut_h;
        self.h = (self.h - cut_h).max(0.0);
        res
    }

    #[inline]
    pub fn cut_bottom(&mut self, amount: f32) -> Slot {
        let cut_h = amount.min(self.h.max(0.0));
        self.h = (self.h - cut_h).max(0.0);
        Slot::new(self.x, self.y + self.h, self.w, cut_h)
    }

    #[inline]
    pub fn pad(&mut self, pad_x: f32, pad_y: f32) {
        self.x += pad_x;
        self.y += pad_y;
        self.w = (self.w - pad_x * 2.0).max(0.0);
        self.h = (self.h - pad_y * 2.0).max(0.0);
    }

    #[inline]
    pub fn remaining(&self) -> Slot {
        Slot::new(self.x, self.y, self.w.max(0.0), self.h.max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rect_cut_left_and_right() {
        let mut r = RectCut::new(10.0, 20.0, 100.0, 50.0);
        let left = r.cut_left(30.0);
        assert_eq!(left.x, 10.0);
        assert_eq!(left.w, 30.0);
        assert_eq!(r.x, 40.0);
        assert_eq!(r.w, 70.0);

        let right = r.cut_right(20.0);
        assert_eq!(right.x, 90.0);
        assert_eq!(right.w, 20.0);
        assert_eq!(r.w, 50.0);

        let rem = r.remaining();
        assert_eq!(rem.x, 40.0);
        assert_eq!(rem.w, 50.0);
    }

    #[test]
    fn test_rect_cut_top_and_bottom() {
        let mut r = RectCut::new(0.0, 0.0, 100.0, 100.0);
        let top = r.cut_top(25.0);
        assert_eq!(top.y, 0.0);
        assert_eq!(top.h, 25.0);
        assert_eq!(r.y, 25.0);
        assert_eq!(r.h, 75.0);

        let bottom = r.cut_bottom(30.0);
        assert_eq!(bottom.y, 70.0);
        assert_eq!(bottom.h, 30.0);
        assert_eq!(r.h, 45.0);
    }

    #[test]
    fn test_rect_cut_pad_and_exhaustion() {
        let mut r = RectCut::new(10.0, 10.0, 100.0, 50.0);
        r.pad(10.0, 5.0);
        assert_eq!(r.x, 20.0);
        assert_eq!(r.y, 15.0);
        assert_eq!(r.w, 80.0);
        assert_eq!(r.h, 40.0);

        // Cutting more than remaining width clamps to 0
        let big = r.cut_left(150.0);
        assert_eq!(big.w, 80.0);
        assert_eq!(r.w, 0.0);
        let nothing = r.cut_left(10.0);
        assert_eq!(nothing.w, 0.0);
    }
}
