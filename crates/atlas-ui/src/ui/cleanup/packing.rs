//! Deterministic, bounded circle packing written for Atlas. No physics or timers.
#[derive(Clone, Copy, Debug)]
pub(super) struct Circle {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub index: usize,
}

pub(super) fn pack(weights: &[u64], width: f32, height: f32) -> Vec<Circle> {
    if width < 4. || height < 4. || !width.is_finite() || !height.is_finite() {
        return Vec::new();
    }
    let mut entries: Vec<(usize, f64)> = weights
        .iter()
        .take(64)
        .enumerate()
        .filter(|(_, w)| **w > 0)
        .map(|(index, w)| (index, (*w as f64).sqrt()))
        .collect();
    if entries.is_empty() {
        return Vec::new();
    }
    entries.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // Gap between neighbours; radii stay exactly proportional to sqrt(weight).
    let gap = entries[0].1 * 0.05;
    // (x, y, padded radius)
    let mut placed: Vec<(f64, f64, f64)> = Vec::with_capacity(entries.len());
    for (k, (_, radius)) in entries.iter().enumerate() {
        let r = radius + gap;
        match k {
            0 => placed.push((0., 0., r)),
            1 => placed.push((placed[0].2 + r, 0., r)),
            _ => {
                let total: f64 = placed.iter().map(|c| c.2 * c.2).sum();
                let (cx, cy) = (
                    placed.iter().map(|c| c.0 * c.2 * c.2).sum::<f64>() / total,
                    placed.iter().map(|c| c.1 * c.2 * c.2).sum::<f64>() / total,
                );
                let mut best: Option<(f64, f64, f64)> = None;
                for i in 0..placed.len() {
                    for j in i + 1..placed.len() {
                        for (x, y) in tangent(placed[i], placed[j], r) {
                            let free = placed
                                .iter()
                                .all(|c| (x - c.0).hypot(y - c.1) >= c.2 + r - 1e-6 * (c.2 + r));
                            let d = (x - cx).hypot(y - cy);
                            if free && best.is_none_or(|b| d < b.2) {
                                best = Some((x, y, d));
                            }
                        }
                    }
                }
                let (x, y) = best.map_or_else(
                    || {
                        let far = placed.iter().map(|c| c.0 + c.2).fold(0., f64::max);
                        (far + r, 0.)
                    },
                    |b| (b.0, b.1),
                );
                placed.push((x, y, r));
            }
        }
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (i, c) in placed.iter().enumerate() {
        let r = entries[i].1;
        x0 = x0.min(c.0 - r);
        y0 = y0.min(c.1 - r);
        x1 = x1.max(c.0 + r);
        y1 = y1.max(c.1 + r);
    }
    let scale = (width as f64 / (x1 - x0)).min(height as f64 / (y1 - y0)) * 0.98;
    let (ox, oy) = (
        width as f64 * 0.5 - (x0 + x1) * 0.5 * scale,
        height as f64 * 0.5 - (y0 + y1) * 0.5 * scale,
    );
    placed
        .iter()
        .enumerate()
        .map(|(i, c)| Circle {
            index: entries[i].0,
            x: (c.0 * scale + ox) as f32,
            y: (c.1 * scale + oy) as f32,
            radius: (entries[i].1 * scale) as f32,
        })
        .collect()
}

/// The two centres at which a circle of radius `r` touches both `a` and `b`.
fn tangent(a: (f64, f64, f64), b: (f64, f64, f64), r: f64) -> [(f64, f64); 2] {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let d = dx.hypot(dy);
    let (ra, rb) = (a.2 + r, b.2 + r);
    if d == 0. || d > ra + rb || d < (ra - rb).abs() {
        return [(f64::NAN, f64::NAN); 2];
    }
    let along = (ra * ra - rb * rb + d * d) / (2. * d);
    let h = (ra * ra - along * along).max(0.).sqrt();
    let (mx, my) = (a.0 + dx * along / d, a.1 + dy * along / d);
    [
        (mx - dy * h / d, my + dx * h / d),
        (mx + dy * h / d, my - dx * h / d),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packing_is_proportional_bounded_and_non_overlapping() {
        for (w, h) in [(700., 450.), (250., 600.), (1200., 200.)] {
            for weights in [vec![64, 16, 4, 1], vec![1; 64], (1..=64).rev().collect()] {
                let circles = pack(&weights, w, h);
                assert_eq!(circles.len(), weights.len());
                for (i, a) in circles.iter().enumerate() {
                    assert!(
                        a.x - a.radius >= 0.
                            && a.x + a.radius <= w
                            && a.y - a.radius >= 0.
                            && a.y + a.radius <= h
                    );
                    for b in &circles[i + 1..] {
                        assert!((a.x - b.x).hypot(a.y - b.y) + 0.001 >= a.radius + b.radius);
                        let ratio = a.radius.powi(2) / b.radius.powi(2);
                        assert!(
                            (ratio - weights[a.index] as f32 / weights[b.index] as f32).abs()
                                < 0.01
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn empty_and_degenerate_viewports_are_safe() {
        assert!(pack(&[], 400., 300.).is_empty());
        assert!(pack(&[0, 0], 400., 300.).is_empty());
        assert!(pack(&[4, 1], 0., 300.).is_empty());
        assert!(pack(&[4, 1], f32::NAN, 300.).is_empty());
    }
}
