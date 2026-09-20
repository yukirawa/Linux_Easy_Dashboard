//! Cairo helpers for the visual widgets.
//!
//! The rule is: **shapes in cairo, text in GTK widgets**. Numbers, captions and
//! headings stay real labels, so typography, accessibility and theming remain
//! native, while cairo does what it is good at — rings, sparklines, bars and
//! glyphs.
//!
//! Colours are never hardcoded. [`Ink`] resolves them through CSS classes, so
//! light/dark themes, custom accents and "reduce motion" all behave.

use gtk::cairo::{Context, LineCap, LineJoin};
use gtk::gdk;
use gtk::prelude::*;

use std::cell::RefCell;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::rc::Rc;

/// Theme colours, resolved through CSS probe labels.
///
/// The probes are attached by [`Ink::wrap`]. They are held through weak
/// references, and the storage itself is shared between clones (`WeakRef::clone`
/// would otherwise freeze whatever the target was at clone time), so a drawing
/// closure can hold an `Ink` without keeping a removed tile alive.
#[derive(Clone)]
pub struct Ink {
    probes: Rc<RefCell<Probes>>,
}

/// The weak references to the probe widgets.
#[derive(Default)]
struct Probes {
    fg: glib::WeakRef<gtk::Widget>,
    accent: glib::WeakRef<gtk::Widget>,
    warn: glib::WeakRef<gtk::Widget>,
    error: glib::WeakRef<gtk::Widget>,
}

impl Ink {
    /// Creates the probes. Call [`Ink::wrap`] to put content and probes into
    /// the widget tree.
    pub fn new() -> Self {
        Self {
            probes: Rc::new(RefCell::new(Probes::default())),
        }
    }

    /// Wraps `content` so the colour probes are inside the widget tree.
    ///
    /// GTK only computes CSS for widgets that are actually visible, so hidden
    /// probes never resolve their colour. These sit in an overlay with zero
    /// opacity instead: they are styled, take no layout space and paint
    /// nothing.
    pub fn wrap(&self, content: &impl IsA<gtk::Widget>) -> gtk::Overlay {
        let probes = gtk::Box::new(gtk::Orientation::Vertical, 0);
        probes.set_halign(gtk::Align::Start);
        probes.set_valign(gtk::Align::Start);
        probes.set_opacity(0.0);
        probes.set_can_target(false);

        {
            let slots = self.probes.borrow();
            for (slot, class) in [
                (&slots.fg, ""),
                (&slots.accent, "edm-ink-accent"),
                (&slots.warn, "edm-ink-warn"),
                (&slots.error, "edm-ink-error"),
            ] {
                let probe = gtk::Box::new(gtk::Orientation::Vertical, 0);
                if !class.is_empty() {
                    probe.add_css_class(class);
                }
                slot.set(Some(probe.upcast_ref::<gtk::Widget>()));
                probes.append(&probe);
            }
        }

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(content));
        overlay.add_overlay(&probes);
        overlay
    }

    fn color(&self, slot: fn(&Probes) -> &glib::WeakRef<gtk::Widget>) -> Option<gdk::RGBA> {
        let probes = self.probes.borrow();
        let widget = slot(&probes).upgrade()?;
        let color = widget.color();
        // A fully transparent probe means the CSS class did not resolve.
        (color.alpha() > 0.0).then_some(color)
    }

    /// The theme's foreground colour. Falls back to white when the probes are
    /// gone, which only happens while tearing a tile down.
    pub fn fg(&self) -> gdk::RGBA {
        self.color(|probes| &probes.fg)
            .unwrap_or_else(|| gdk::RGBA::new(1.0, 1.0, 1.0, 1.0))
    }

    pub fn accent(&self) -> gdk::RGBA {
        self.color(|probes| &probes.accent)
            .unwrap_or_else(|| self.fg())
    }

    pub fn warn(&self) -> gdk::RGBA {
        self.color(|probes| &probes.warn)
            .unwrap_or_else(|| self.fg())
    }

    pub fn error(&self) -> gdk::RGBA {
        self.color(|probes| &probes.error)
            .unwrap_or_else(|| self.fg())
    }

    /// Foreground at `alpha`: tracks, grid lines, secondary text.
    pub fn alpha(&self, alpha: f32) -> gdk::RGBA {
        let mut color = self.fg();
        color.set_alpha(alpha);
        color
    }

    /// Accent, then warning, then error as `fraction` grows — the usual
    /// "am I running out of something" colour ramp.
    pub fn level(&self, fraction: f64) -> gdk::RGBA {
        if fraction >= 0.9 {
            self.error()
        } else if fraction >= 0.75 {
            self.warn()
        } else {
            self.accent()
        }
    }
}

impl Default for Ink {
    fn default() -> Self {
        Self::new()
    }
}

/// A rectangle in the widget's own coordinates.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Bounds {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub const fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn shortest_side(&self) -> f64 {
        self.w.min(self.h)
    }
}

/// Prepares the context for the visual language used everywhere: rounded caps,
/// round joins, no leftover path.
pub fn prepare(cr: &Context) {
    cr.set_line_cap(LineCap::Round);
    cr.set_line_join(LineJoin::Round);
    cr.new_path();
}

pub fn set_color(cr: &Context, color: &gdk::RGBA) {
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
}

/// Adds a rounded rectangle to the current path.
pub fn rounded_rect_path(cr: &Context, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    let radius = radius.max(0.0).min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - radius, y + radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(x + w - radius, y + h - radius, radius, 0.0, FRAC_PI_2);
    cr.arc(x + radius, y + h - radius, radius, FRAC_PI_2, PI);
    cr.arc(x + radius, y + radius, radius, PI, PI + FRAC_PI_2);
    cr.close_path();
}

/// Fills a rounded rectangle.
pub fn fill_rounded(cr: &Context, x: f64, y: f64, w: f64, h: f64, radius: f64, color: &gdk::RGBA) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    rounded_rect_path(cr, x, y, w, h, radius);
    set_color(cr, color);
    let _ = cr.fill();
    cr.new_path();
}

/// Draws a filled circle.
pub fn disc(cr: &Context, cx: f64, cy: f64, radius: f64, color: &gdk::RGBA) {
    if radius <= 0.0 {
        return;
    }
    cr.new_path();
    cr.arc(cx, cy, radius, 0.0, TAU);
    set_color(cr, color);
    let _ = cr.fill();
}

/// Draws a straight line with rounded caps.
pub fn line(cr: &Context, from: (f64, f64), to: (f64, f64), width: f64, color: &gdk::RGBA) {
    cr.new_path();
    cr.move_to(from.0, from.1);
    cr.line_to(to.0, to.1);
    cr.set_line_width(width);
    set_color(cr, color);
    let _ = cr.stroke();
}

/// Draws an arc (a piece of a ring) around `center`, starting at 12 o'clock.
pub fn arc(
    cr: &Context,
    center: (f64, f64),
    radius: f64,
    from: f64,
    to: f64,
    width: f64,
    color: &gdk::RGBA,
) {
    cr.new_path();
    cr.arc(center.0, center.1, radius, from, to);
    cr.set_line_width(width);
    set_color(cr, color);
    let _ = cr.stroke();
}

/// Ring gauge: a full track plus a value arc drawn clockwise from 12 o'clock.
pub fn ring(
    cr: &Context,
    center: (f64, f64),
    radius: f64,
    thickness: f64,
    fraction: f64,
    track: &gdk::RGBA,
    value: &gdk::RGBA,
) {
    if radius <= 0.0 || thickness <= 0.0 {
        return;
    }
    arc(cr, center, radius, 0.0, TAU, thickness, track);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.001 {
        let start = -FRAC_PI_2;
        arc(
            cr,
            center,
            radius,
            start,
            start + fraction * TAU,
            thickness,
            value,
        );
    }
}

/// Horizontal progress bar with a rounded track.
pub fn bar(cr: &Context, bounds: Bounds, fraction: f64, track: &gdk::RGBA, value: &gdk::RGBA) {
    let radius = bounds.h / 2.0;
    fill_rounded(cr, bounds.x, bounds.y, bounds.w, bounds.h, radius, track);
    if fraction > 0.001 {
        let filled = (bounds.w * fraction.clamp(0.0, 1.0)).max(bounds.h.min(bounds.w));
        fill_rounded(cr, bounds.x, bounds.y, filled, bounds.h, radius, value);
    }
}

/// Sparkline: the area under the curve, then the curve itself.
///
/// `values` is oldest first, `max` the top of the scale (`None` uses the
/// maximum of the values, which makes quiet periods look dramatic).
pub fn sparkline(
    cr: &Context,
    bounds: Bounds,
    values: &[f64],
    max: Option<f64>,
    color: &gdk::RGBA,
) {
    if values.len() < 2 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return;
    }
    let max = max
        .unwrap_or_else(|| values.iter().copied().fold(0.0, f64::max))
        .max(0.0001);
    let step = bounds.w / (values.len() - 1) as f64;
    let point = |index: usize, value: f64| {
        (
            bounds.x + step * index as f64,
            bounds.y + bounds.h - (value.clamp(0.0, max) / max) * bounds.h,
        )
    };

    // Soft fill under the curve.
    let mut fill = *color;
    fill.set_alpha(0.22);
    cr.new_path();
    cr.move_to(bounds.x, bounds.y + bounds.h);
    for (index, value) in values.iter().enumerate() {
        let (px, py) = point(index, *value);
        cr.line_to(px, py);
    }
    cr.line_to(bounds.x + bounds.w, bounds.y + bounds.h);
    cr.close_path();
    set_color(cr, &fill);
    let _ = cr.fill();

    // The curve.
    cr.new_path();
    for (index, value) in values.iter().enumerate() {
        let (px, py) = point(index, *value);
        if index == 0 {
            cr.move_to(px, py);
        } else {
            cr.line_to(px, py);
        }
    }
    cr.set_line_width(2.0);
    set_color(cr, color);
    let _ = cr.stroke();
    cr.new_path();
}

/// Column chart, oldest first. Used for per-core CPU load.
pub fn columns(
    cr: &Context,
    bounds: Bounds,
    values: &[f64],
    max: f64,
    gap: f64,
    track: &gdk::RGBA,
    colors: &[gdk::RGBA],
) {
    if values.is_empty() || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return;
    }
    let slot = bounds.w / values.len() as f64;
    let bar_width = (slot - gap).max(1.0);
    let max = max.max(0.0001);
    let radius = (bar_width / 2.0).min(bounds.h / 2.0);

    for (index, value) in values.iter().enumerate() {
        let bx = bounds.x + slot * index as f64;
        let fraction = (value / max).clamp(0.0, 1.0);
        fill_rounded(cr, bx, bounds.y, bar_width, bounds.h, radius, track);
        let height = (bounds.h * fraction).max(radius * 2.0).min(bounds.h);
        let color = colors
            .get(index)
            .copied()
            .unwrap_or_else(|| colors.first().copied().unwrap_or(*track));
        fill_rounded(
            cr,
            bx,
            bounds.y + bounds.h - height,
            bar_width,
            height,
            radius,
            &color,
        );
    }
}

/// Weather symbols, drawn rather than taken from the icon theme so they look
/// consistent in every icon theme and always follow the widget's colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherGlyph {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Drizzle,
    Rain,
    Snow,
    Thunder,
}

/// Draws `glyph` centred in the given box. `ink` is used for cloud and rain,
/// `accent` for the sun.
pub fn weather_glyph(
    cr: &Context,
    bounds: Bounds,
    glyph: WeatherGlyph,
    ink: &gdk::RGBA,
    accent: &gdk::RGBA,
) {
    let size = bounds.shortest_side();
    if size <= 4.0 {
        return;
    }
    let (cx, cy) = bounds.center();
    let unit = size / 16.0;
    let faint = alpha_of(ink, 0.25);

    match glyph {
        WeatherGlyph::Clear => {
            sun(cr, cx, cy, unit * 4.0, accent);
        }
        WeatherGlyph::PartlyCloudy => {
            sun(cr, cx + unit * 3.0, cy - unit * 3.0, unit * 3.0, accent);
            cloud(cr, cx - unit * 1.0, cy + unit * 1.5, unit * 5.5, ink);
        }
        WeatherGlyph::Cloudy => {
            cloud(cr, cx - unit * 2.0, cy - unit * 1.0, unit * 4.5, &faint);
            cloud(cr, cx + unit * 0.5, cy + unit * 1.0, unit * 5.5, ink);
        }
        WeatherGlyph::Fog => {
            cloud(cr, cx, cy - unit * 2.0, unit * 5.0, ink);
            for row in 0..3 {
                let ly = cy + unit * (1.0 + row as f64 * 1.6);
                let inset = unit * (1.5 - row as f64 * 0.4);
                line(
                    cr,
                    (cx - unit * 5.0 + inset, ly),
                    (cx + unit * 5.0 - inset, ly),
                    unit * 0.9,
                    &faint,
                );
            }
        }
        WeatherGlyph::Drizzle | WeatherGlyph::Rain => {
            cloud(cr, cx, cy - unit * 2.0, unit * 5.0, ink);
            let drops = if glyph == WeatherGlyph::Rain { 3 } else { 2 };
            for index in 0..drops {
                let dx = cx + unit * (index as f64 - 1.0) * 2.4;
                let bottom = cy + unit * 4.5;
                line(
                    cr,
                    (dx, cy + unit * 1.2),
                    (dx - unit * 0.8, bottom),
                    unit * 1.1,
                    accent,
                );
            }
        }
        WeatherGlyph::Snow => {
            cloud(cr, cx, cy - unit * 2.0, unit * 5.0, ink);
            for index in 0..3 {
                let dx = cx + unit * (index as f64 - 1.0) * 2.4;
                let dy = cy + unit * 3.0;
                let r = unit * 0.8;
                line(cr, (dx - r, dy), (dx + r, dy), unit * 0.9, accent);
                line(cr, (dx, dy - r), (dx, dy + r), unit * 0.9, accent);
            }
        }
        WeatherGlyph::Thunder => {
            cloud(cr, cx, cy - unit * 2.0, unit * 5.0, ink);
            cr.new_path();
            cr.move_to(cx + unit * 0.8, cy + unit * 0.6);
            cr.line_to(cx - unit * 0.9, cy + unit * 3.4);
            cr.line_to(cx + unit * 0.4, cy + unit * 3.1);
            cr.line_to(cx - unit * 0.3, cy + unit * 5.4);
            cr.line_to(cx + unit * 1.6, cy + unit * 1.9);
            cr.line_to(cx + unit * 0.2, cy + unit * 2.2);
            cr.close_path();
            set_color(cr, accent);
            let _ = cr.fill();
        }
    }
    prepare(cr);
}

fn sun(cr: &Context, cx: f64, cy: f64, radius: f64, color: &gdk::RGBA) {
    disc(cr, cx, cy, radius, color);
    let ray_inner = radius * 1.5;
    let ray_outer = radius * 2.1;
    for step in 0..8 {
        let angle = TAU * step as f64 / 8.0;
        let (sin, cos) = angle.sin_cos();
        line(
            cr,
            (cx + cos * ray_inner, cy + sin * ray_inner),
            (cx + cos * ray_outer, cy + sin * ray_outer),
            (radius * 0.28).max(1.0),
            color,
        );
    }
}

/// A three-circle cloud, which reads well at small sizes.
fn cloud(cr: &Context, cx: f64, cy: f64, radius: f64, color: &gdk::RGBA) {
    disc(
        cr,
        cx - radius * 0.75,
        cy + radius * 0.15,
        radius * 0.7,
        color,
    );
    disc(
        cr,
        cx + radius * 0.7,
        cy + radius * 0.1,
        radius * 0.6,
        color,
    );
    disc(
        cr,
        cx - radius * 0.05,
        cy - radius * 0.35,
        radius * 0.85,
        color,
    );
    fill_rounded(
        cr,
        cx - radius * 0.95,
        cy,
        radius * 1.85,
        radius * 0.75,
        radius * 0.35,
        color,
    );
}

fn alpha_of(color: &gdk::RGBA, alpha: f32) -> gdk::RGBA {
    let mut color = *color;
    color.set_alpha(alpha);
    color
}

/// Tick marks around a dial, with every `major_every`-th tick longer and
/// brighter. Used by the analog clock.
pub fn dial_ticks(
    cr: &Context,
    center: (f64, f64),
    radius: f64,
    count: usize,
    major_every: usize,
    ink: &Ink,
) {
    let (cx, cy) = center;
    let minor = ink.alpha(0.25);
    let major = ink.alpha(0.65);
    for step in 0..count {
        let angle = TAU * step as f64 / count as f64 - FRAC_PI_2;
        let (sin, cos) = angle.sin_cos();
        let is_major = step % major_every == 0;
        let outer = radius;
        let inner = radius
            - if is_major {
                radius * 0.13
            } else {
                radius * 0.07
            };
        let (width, color) = if is_major {
            (radius * 0.035, major)
        } else {
            (radius * 0.018, minor)
        };
        line(
            cr,
            (cx + cos * inner, cy + sin * inner),
            (cx + cos * outer, cy + sin * outer),
            width.max(1.0),
            &color,
        );
    }
}
