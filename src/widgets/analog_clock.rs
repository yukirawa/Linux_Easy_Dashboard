//! Analog clock: a cairo drawn dial, square by default (aspect ratio locked).
//!
//! Text is limited to the hour numerals, which are ASCII digits, so cairo's toy
//! text API is enough — everything else is a real GTK label or a drawn shape.

use std::cell::RefCell;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::{Context, FontSlant, FontWeight};
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics;
use crate::graphics::Ink;

use super::{Category, DetailPage, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "analog_clock";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "アナログ時計",
    summary: "文字盤の時計（正方形）",
    icon: "alarm-symbolic",
    category: Category::Time,
    default_size: (260, 260),
    aspect: Some(1.0),
    build,
    detail,
};

/// How often the dial is redrawn.
const TICK_CALM: u32 = 1000;
const TICK_SMOOTH: u32 = 50;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct AnalogConfig {
    show_seconds: bool,
    show_numerals: bool,
    /// Move the second hand continuously instead of once per second. Costs a
    /// redraw 20 times a second, so it is off by default.
    smooth_seconds: bool,
}

impl Default for AnalogConfig {
    fn default() -> Self {
        Self {
            show_seconds: true,
            show_numerals: true,
            smooth_seconds: false,
        }
    }
}

/// The hand angles, in turns from 12 o'clock.
#[derive(Debug, Clone, Copy, Default)]
struct Hands {
    hour: f64,
    minute: f64,
    second: f64,
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: AnalogConfig = context.config();

    let hands = Rc::new(RefCell::new(Hands::default()));
    let ink = Ink::new();
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_content_width(120);
    area.set_content_height(120);

    {
        let hands = hands.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            let hands = *hands.borrow();
            draw_face(
                cr,
                f64::from(width),
                f64::from(height),
                &hands,
                &ink,
                config,
            );
        });
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&area);
    let root = ink.wrap(&root);

    let tick = if config.smooth_seconds && config.show_seconds {
        TICK_SMOOTH
    } else {
        TICK_CALM
    };
    // Without a second hand the dial only changes once a minute, so skip the
    // redraws in between.
    let smooth = config.smooth_seconds && config.show_seconds;
    let last = Rc::new(std::cell::Cell::new((-1, -1, -1)));
    let area_for_tick = area.clone();
    super::tick_millis(&area, tick, move || {
        let Ok(now) = glib::DateTime::now_local() else {
            return;
        };
        let key = (now.hour(), now.minute(), now.second());
        if !smooth && last.get() == key {
            return;
        }
        last.set(key);
        *hands.borrow_mut() = hands_of(&now);
        area_for_tick.queue_draw();
    });

    Ok(root.upcast())
}

/// The hand angles for a moment in time, in turns from 12 o'clock.
fn hands_of(now: &glib::DateTime) -> Hands {
    let hour = f64::from(now.hour());
    let minute = f64::from(now.minute());
    // `seconds()` carries the fraction, which is what makes a smooth sweep.
    let second = now.seconds();
    Hands {
        hour: ((hour % 12.0) + minute / 60.0 + second / 3600.0) / 12.0,
        minute: (minute + second / 60.0) / 60.0,
        second: second / 60.0,
    }
}

fn draw_face(
    cr: &Context,
    width: f64,
    height: f64,
    hands: &Hands,
    ink: &Ink,
    config: AnalogConfig,
) {
    graphics::prepare(cr);

    let size = width.min(height);
    if size < 24.0 {
        return;
    }
    let (cx, cy) = (width / 2.0, height / 2.0);
    let radius = size / 2.0 - size * 0.02;
    let track = ink.alpha(0.15);
    let face = ink.fg();

    // Dial ring and ticks.
    graphics::arc(
        cr,
        (cx, cy),
        radius,
        0.0,
        TAU,
        (size * 0.012).max(1.0),
        &track,
    );
    graphics::dial_ticks(cr, (cx, cy), radius - size * 0.035, 60, 5, ink);

    if config.show_numerals && size >= 130.0 {
        draw_numerals(cr, cx, cy, radius * 0.76, size * 0.11, &face);
    }

    // Hour hand: shorter and heavier.
    hand(cr, cx, cy, hands.hour, radius * 0.5, size * 0.035, &face);
    // Minute hand.
    hand(cr, cx, cy, hands.minute, radius * 0.74, size * 0.022, &face);
    // Second hand in the accent colour, with a small counterweight.
    if config.show_seconds {
        let accent = ink.accent();
        let angle = hands.second * TAU - FRAC_PI_2;
        let (sin, cos) = angle.sin_cos();
        graphics::line(
            cr,
            (cx - cos * radius * 0.14, cy - sin * radius * 0.14),
            (cx + cos * radius * 0.84, cy + sin * radius * 0.84),
            (size * 0.012).max(1.0),
            &accent,
        );
    }

    // Centre pin on top of the hands: a soft disc with an accent dot.
    graphics::disc(cr, cx, cy, (size * 0.028).max(1.5), &face);
    graphics::disc(cr, cx, cy, (size * 0.012).max(0.8), &ink.accent());
}

fn hand(
    cr: &Context,
    cx: f64,
    cy: f64,
    turns: f64,
    length: f64,
    width: f64,
    color: &gtk::gdk::RGBA,
) {
    let angle = turns * TAU - FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    // Start slightly behind the centre so the hands do not form a gap.
    let tail = length * 0.12;
    graphics::line(
        cr,
        (cx - cos * tail, cy - sin * tail),
        (cx + cos * length, cy + sin * length),
        width.max(1.0),
        color,
    );
}

fn draw_numerals(
    cr: &Context,
    cx: f64,
    cy: f64,
    radius: f64,
    font_size: f64,
    color: &gtk::gdk::RGBA,
) {
    cr.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
    cr.set_font_size(font_size);
    graphics::set_color(cr, color);

    for hour in 1..=12 {
        let text = hour.to_string();
        let angle = TAU * f64::from(hour) / 12.0 - FRAC_PI_2;
        let (sin, cos) = angle.sin_cos();
        let x = cx + cos * radius;
        let y = cy + sin * radius;

        let Ok(extents) = cr.text_extents(&text) else {
            continue;
        };
        cr.move_to(
            x - (extents.width() / 2.0 + extents.x_bearing()),
            y - (extents.height() / 2.0 + extents.y_bearing()),
        );
        if let Err(e) = cr.show_text(&text) {
            log::debug!("文字盤の数字を描けません: {e}");
            return;
        }
    }
    cr.new_path();
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: AnalogConfig = context.config();
    let page = DetailPage::new("時計", "表示");
    page.switch(context, "show_seconds", "秒針を表示", config.show_seconds);
    page.switch(context, "show_numerals", "数字を表示", config.show_numerals);
    page.switch(
        context,
        "smooth_seconds",
        "秒針を滑らかに動かす",
        config.smooth_seconds,
    );
    page.note("滑らかな秒針は毎秒 20 回描画するため、消費電力が少し増えます。");
    page.note("正方形を保つには、右クリックメニューの「縦横比を固定」を有効にしてください。");
    Ok(page.finish())
}
