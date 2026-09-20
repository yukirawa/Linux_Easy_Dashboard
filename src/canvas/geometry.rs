//! Geometry and snapping rules for the canvas.
//!
//! Deliberately free of GTK types so the layout behaviour can be unit tested
//! without a display server.

/// An axis aligned rectangle in canvas coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub const fn center_x(self) -> i32 {
        self.x + self.w / 2
    }

    pub const fn center_y(self) -> i32 {
        self.y + self.h / 2
    }

    pub const fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// Is `(x, y)` inside the bottom right resize corner?
    pub const fn contains_grip(self, x: i32, y: i32, size: i32) -> bool {
        x >= self.right() - size
            && x < self.right()
            && y >= self.bottom() - size
            && y < self.bottom()
    }

    pub const fn with_min_size(self, min_w: i32, min_h: i32) -> Self {
        let w = if self.w < min_w { min_w } else { self.w };
        let h = if self.h < min_h { min_h } else { self.h };
        Self { w, h, ..self }
    }

    pub const fn at_least_origin(self) -> Self {
        let x = if self.x < 0 { 0 } else { self.x };
        let y = if self.y < 0 { 0 } else { self.y };
        Self { x, y, ..self }
    }

    /// Interpolates towards `other`; `t` may leave `0.0..=1.0` on purpose so a
    /// spring can overshoot.
    pub fn lerp(self, other: Self, t: f64) -> Self {
        let mix =
            |a: i32, b: i32| (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as i32;
        Self {
            x: mix(self.x, other.x),
            y: mix(self.y, other.y),
            w: mix(self.w, other.w),
            h: mix(self.h, other.h),
        }
    }

    /// Same rectangle, scaled around its own centre.
    pub fn scaled_about_center(self, factor: f64) -> Self {
        let w = ((self.w as f64) * factor).round() as i32;
        let h = ((self.h as f64) * factor).round() as i32;
        Self {
            x: self.x + (self.w - w) / 2,
            y: self.y + (self.h - h) / 2,
            w,
            h,
        }
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// An alignment line drawn while dragging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guide {
    Vertical(i32),
    Horizontal(i32),
}

/// A snap result: the adjusted rectangle plus the guides that caused it.
#[derive(Debug, Clone, Default)]
pub struct Snap {
    pub rect: Rect,
    pub guides: Vec<Guide>,
}

/// Everything the snapping engine may align against.
#[derive(Debug, Clone, Default)]
pub struct SnapTargets {
    pub grid: i32,
    pub threshold: i32,
    pub bounds: (i32, i32),
    /// Rectangles of the other widgets on the canvas.
    pub others: Vec<Rect>,
}

impl SnapTargets {
    fn x_candidates(&self) -> Vec<i32> {
        let mut out = vec![0, self.bounds.0 / 2, self.bounds.0];
        for r in &self.others {
            out.extend([r.x, r.center_x(), r.right()]);
        }
        out
    }

    fn y_candidates(&self) -> Vec<i32> {
        let mut out = vec![0, self.bounds.1 / 2, self.bounds.1];
        for r in &self.others {
            out.extend([r.y, r.center_y(), r.bottom()]);
        }
        out
    }
}

/// Finds the closest alignment of any `anchor` onto any `candidate`, returning
/// `(delta, line)` where `delta` moves the anchor onto `line`.
fn axis_align(anchors: &[i32], candidates: &[i32], threshold: i32) -> Option<(i32, i32)> {
    let mut best: Option<(i32, i32, i32)> = None;
    for anchor in anchors {
        for candidate in candidates {
            let distance = (candidate - anchor).abs();
            if distance <= threshold && best.is_none_or(|(bd, _, _)| distance < bd) {
                best = Some((distance, candidate - anchor, *candidate));
            }
        }
    }
    best.map(|(_, delta, line)| (delta, line))
}

fn grid_round(value: i32, grid: i32) -> i32 {
    if grid <= 1 {
        return value;
    }
    let grid = f64::from(grid);
    ((f64::from(value) / grid).round() * grid) as i32
}

/// Snaps a moved rectangle, keeping its size.
pub fn snap_move(raw: Rect, targets: &SnapTargets) -> Snap {
    let mut rect = raw.at_least_origin();
    let mut guides = Vec::new();

    let anchors_x = [rect.x, rect.center_x(), rect.right()];
    if let Some((delta, line)) = axis_align(&anchors_x, &targets.x_candidates(), targets.threshold)
    {
        rect.x += delta;
        guides.push(Guide::Vertical(line));
    } else {
        let snapped = grid_round(rect.x, targets.grid);
        if snapped != rect.x {
            rect.x = snapped;
            guides.push(Guide::Vertical(rect.x));
        }
    }

    let anchors_y = [rect.y, rect.center_y(), rect.bottom()];
    if let Some((delta, line)) = axis_align(&anchors_y, &targets.y_candidates(), targets.threshold)
    {
        rect.y += delta;
        guides.push(Guide::Horizontal(line));
    } else {
        let snapped = grid_round(rect.y, targets.grid);
        if snapped != rect.y {
            rect.y = snapped;
            guides.push(Guide::Horizontal(rect.y));
        }
    }

    Snap { rect, guides }
}

/// Snaps a rectangle that is being resized from its bottom right corner.
///
/// When `aspect` is set the height is derived from the width so the ratio is
/// preserved exactly.
pub fn snap_resize(raw: Rect, targets: &SnapTargets, aspect: Option<f64>, min: (i32, i32)) -> Snap {
    let mut rect = raw.with_min_size(min.0.max(1), min.1.max(1));
    let mut guides = Vec::new();

    if let Some((delta, line)) =
        axis_align(&[rect.right()], &targets.x_candidates(), targets.threshold)
    {
        rect.w = (rect.w + delta).max(min.0.max(1));
        guides.push(Guide::Vertical(line));
    } else {
        let snapped = grid_round(rect.w, targets.grid);
        if snapped != rect.w {
            rect.w = snapped.max(min.0.max(1));
        }
    }

    if let Some(aspect) = aspect.filter(|a| *a > 0.0) {
        rect.h = ((rect.w as f64) / aspect).round() as i32;
    } else if let Some((delta, line)) =
        axis_align(&[rect.bottom()], &targets.y_candidates(), targets.threshold)
    {
        rect.h = (rect.h + delta).max(min.1.max(1));
        guides.push(Guide::Horizontal(line));
    } else {
        let snapped = grid_round(rect.h, targets.grid);
        if snapped != rect.h {
            rect.h = snapped.max(min.1.max(1));
        }
    }

    Snap { rect, guides }
}

/// First position of a `size`-sized widget that does not overlap `occupied`.
///
/// Scans in reading order with a `step`, then falls back to stacking below
/// everything else.
pub fn free_spot(size: (i32, i32), occupied: &[Rect], area: (i32, i32), step: i32) -> (i32, i32) {
    let step = step.max(1);
    let candidate = Rect::new(0, 0, size.0, size.1);
    let mut y = MARGIN;
    while y + size.1 <= area.1.max(MARGIN + size.1) {
        let mut x = MARGIN;
        while x + size.0 <= area.0.max(MARGIN + size.0) {
            let probe = Rect { x, y, ..candidate };
            if !occupied.iter().any(|r| r.intersects(probe)) {
                return (x, y);
            }
            x += step;
        }
        y += step;
    }

    let bottom = occupied.iter().map(|r| r.bottom()).max().unwrap_or(0);
    (MARGIN, bottom + MARGIN)
}

const MARGIN: i32 = 24;

#[cfg(test)]
mod tests {
    use super::*;

    fn targets() -> SnapTargets {
        SnapTargets {
            grid: 16,
            threshold: 8,
            bounds: (1000, 800),
            others: vec![Rect::new(100, 100, 200, 100)],
        }
    }

    #[test]
    fn rect_helpers() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!(r.right(), 40);
        assert_eq!(r.bottom(), 60);
        assert!(r.contains(10, 20));
        assert!(!r.contains(40, 60));
        assert!(r.contains_grip(39, 59, 18));
        assert!(!r.contains_grip(20, 20, 18));
        assert_eq!(r.with_min_size(100, 100), Rect::new(10, 20, 100, 100));
        assert_eq!(
            Rect::new(-5, -9, 4, 4).at_least_origin(),
            Rect::new(0, 0, 4, 4)
        );
    }

    #[test]
    fn lerp_endpoints_are_exact() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 60, 200, 300);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
    }

    #[test]
    fn move_snaps_to_neighbour_edge() {
        let raw = Rect::new(103, 300, 200, 100);
        let snap = snap_move(raw, &targets());
        assert_eq!(
            snap.rect.x, 100,
            "left edge should align with the neighbour"
        );
        assert!(snap.guides.contains(&Guide::Vertical(100)));
    }

    #[test]
    fn move_falls_back_to_grid() {
        let raw = Rect::new(203, 302, 50, 50);
        let mut t = targets();
        t.others.clear();
        let snap = snap_move(raw, &t);
        assert_eq!(snap.rect.x % 16, 0);
        assert_eq!(snap.rect.y % 16, 0);
    }

    #[test]
    fn move_is_clamped_to_origin() {
        let raw = Rect::new(-40, -40, 100, 100);
        let snap = snap_move(raw, &targets());
        assert!(snap.rect.x >= 0 && snap.rect.y >= 0);
    }

    #[test]
    fn resize_respects_aspect() {
        let raw = Rect::new(0, 0, 300, 150);
        let mut t = targets();
        t.others.clear();
        t.grid = 0;
        let snap = snap_resize(raw, &t, Some(2.0), (100, 50));
        assert_eq!(snap.rect.h, snap.rect.w / 2);
    }

    #[test]
    fn resize_never_goes_below_minimum() {
        let raw = Rect::new(0, 0, 5, 5);
        let mut t = targets();
        t.others.clear();
        let snap = snap_resize(raw, &t, None, (120, 80));
        assert!(snap.rect.w >= 120 && snap.rect.h >= 80);
    }

    #[test]
    fn free_spot_avoids_occupied_area() {
        let occupied = vec![Rect::new(24, 24, 300, 200)];
        let (x, y) = free_spot((300, 200), &occupied, (800, 600), 16);
        let probe = Rect::new(x, y, 300, 200);
        assert!(!probe.intersects(occupied[0]));
    }
}
