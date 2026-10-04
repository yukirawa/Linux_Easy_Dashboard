//! `time` view family. See `src/views/mod.rs`.
//!
//! Clocks and calendars: every renderer here is driven by `glib::DateTime`, so
//! `TZ`, `LC_TIME` and the system timezone all just work. Instance overrides
//! are already applied to `definition.view` by the caller.

use std::cell::RefCell;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::{Context, FontSlant, FontWeight};
use gtk::prelude::*;

use crate::graphics::{self, Ink};
use crate::plugin::{Plugin, View};
use crate::widgets::{WidgetContext, tick_millis, tick_seconds};

fn time_format(hour24: bool, show_seconds: bool) -> &'static str {
    match (hour24, show_seconds) {
        (true, true) => "%H:%M:%S",
        (true, false) => "%H:%M",
        (false, true) => "%I:%M:%S %p",
        (false, false) => "%I:%M %p",
    }
}

fn now_in(timezone: &str) -> Option<glib::DateTime> {
    let zone = if timezone.trim().is_empty() {
        glib::TimeZone::local()
    } else {
        glib::TimeZone::new(Some(timezone.trim()))
    };
    glib::DateTime::now(&zone).ok()
}

fn format_opt(now: Option<&glib::DateTime>, format: &str) -> String {
    now.and_then(|now| now.format(format).ok())
        .map_or_else(|| "--:--".to_owned(), |s| s.to_string())
}

// ---------------------------------------------------------------------------
// clock
// ---------------------------------------------------------------------------

pub fn clock(definition: &Plugin, _context: &WidgetContext) -> Result<gtk::Widget> {
    let (hour24, show_seconds, show_date, timezone) = match &definition.view {
        View::Clock {
            hour24,
            show_seconds,
            show_date,
            timezone,
        } => (*hour24, *show_seconds, *show_date, timezone.clone()),
        _ => unreachable!("clock called with the wrong view"),
    };

    let time = gtk::Label::new(None);
    time.add_css_class("edm-time");
    time.set_xalign(0.0);
    time.set_halign(gtk::Align::Center);

    let date = gtk::Label::new(None);
    date.add_css_class("dim-label");
    date.set_halign(gtk::Align::Center);
    date.set_visible(show_date);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
    body.set_valign(gtk::Align::Center);
    body.set_vexpand(true);
    body.append(&time);
    body.append(&date);

    let refresh = {
        let time = time.clone();
        let date = date.clone();
        let timezone = timezone.clone();
        move || {
            let now = now_in(&timezone);
            let text = format_opt(now.as_ref(), time_format(hour24, show_seconds));
            if time.label() != text {
                time.set_label(&text);
            }
            let date_text = if show_date {
                format_opt(now.as_ref(), "%Y-%m-%d (%a)")
            } else {
                String::new()
            };
            if date.label() != date_text {
                date.set_label(&date_text);
            }
        }
    };
    refresh();
    tick_seconds(&body, 1, refresh);
    Ok(body.upcast())
}

// ---------------------------------------------------------------------------
// analog_clock
// ---------------------------------------------------------------------------

struct Hands {
    hour: f64,
    minute: f64,
    second: f64,
}

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
    let tail = length * 0.12;
    graphics::line(
        cr,
        (cx - cos * tail, cy - sin * tail),
        (cx + cos * length, cy + sin * length),
        width.max(1.0),
        color,
    );
}

pub fn analog_clock(definition: &Plugin, _context: &WidgetContext) -> Result<gtk::Widget> {
    let (show_seconds, show_numerals, smooth_seconds) = match &definition.view {
        View::AnalogClock {
            show_seconds,
            show_numerals,
            smooth_seconds,
        } => (*show_seconds, *show_numerals, *smooth_seconds),
        _ => unreachable!("analog_clock called with the wrong view"),
    };

    let ink = Ink::new();
    let area = gtk::DrawingArea::new();
    area.set_content_width(200);
    area.set_content_height(200);
    area.set_hexpand(true);
    area.set_vexpand(true);

    // A 1 s tick is enough unless the second hand is meant to sweep.
    let smooth = smooth_seconds && show_seconds;
    let ticks = if smooth { 50 } else { 1000 };

    {
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            let now = glib::DateTime::now_local().ok();
            let hands = now.as_ref().map(hands_of).unwrap_or(Hands {
                hour: 0.0,
                minute: 0.0,
                second: 0.0,
            });
            draw_face(
                cr,
                f64::from(width),
                f64::from(height),
                &hands,
                &ink,
                show_seconds,
                show_numerals,
            );
        });
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    root.append(&area);
    let root = ink.wrap(&root);

    let area_for_tick = area.clone();
    if smooth {
        tick_millis(&root, ticks, move || area_for_tick.queue_draw());
    } else {
        tick_seconds(&root, 1, move || area_for_tick.queue_draw());
    }
    Ok(root.upcast())
}

fn draw_face(
    cr: &Context,
    width: f64,
    height: f64,
    hands: &Hands,
    ink: &Ink,
    show_seconds: bool,
    show_numerals: bool,
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

    if show_numerals && size >= 130.0 {
        draw_numerals(cr, cx, cy, radius * 0.76, size * 0.11, &face);
    }

    hand(cr, cx, cy, hands.hour, radius * 0.5, size * 0.035, &face);
    hand(cr, cx, cy, hands.minute, radius * 0.74, size * 0.022, &face);
    if show_seconds {
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

    graphics::disc(cr, cx, cy, (size * 0.028).max(1.5), &face);
    graphics::disc(cr, cx, cy, (size * 0.012).max(0.8), &ink.accent());
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

// ---------------------------------------------------------------------------
// world_clock
// ---------------------------------------------------------------------------

struct ZoneRow {
    name: gtk::Label,
    time: gtk::Label,
    dot: gtk::DrawingArea,
    zone: String,
}

fn city_of(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

pub fn world_clock(definition: &Plugin, _context: &WidgetContext) -> Result<gtk::Widget> {
    let (zones, hour24) = match &definition.view {
        View::WorldClock { zones, hour24 } => (super::csv(zones), *hour24),
        _ => unreachable!("world_clock called with the wrong view"),
    };

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_vexpand(true);
    root.set_valign(gtk::Align::Center);

    if zones.is_empty() {
        let hint = gtk::Label::new(Some("タイムゾーンを設定してください"));
        hint.add_css_class("dim-label");
        root.append(&hint);
        return Ok(root.upcast());
    }

    let mut rows: Vec<ZoneRow> = Vec::new();
    for zone in zones {
        let name = gtk::Label::new(Some(&city_of(&zone)));
        name.set_xalign(0.0);
        name.add_css_class("heading");

        let time = gtk::Label::new(None);
        time.set_xalign(1.0);
        time.add_css_class("edm-time");

        let dot = gtk::DrawingArea::new();
        dot.set_content_width(10);
        dot.set_content_height(10);
        dot.set_valign(gtk::Align::Center);

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.append(&name);
        line.append(&time);
        line.append(&dot);
        root.append(&line);

        rows.push(ZoneRow {
            name,
            time,
            dot,
            zone,
        });
    }

    let rows = Rc::new(RefCell::new(rows));
    let refresh = {
        let rows = Rc::clone(&rows);
        move || {
            for row in rows.borrow().iter() {
                let Some(datetime) = now_in(&row.zone) else {
                    row.time.set_label("--:--");
                    row.name.set_tooltip_text(None);
                    continue;
                };
                let format = if hour24 { "%H:%M" } else { "%I:%M %p" };
                let text = datetime
                    .format(format)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| "--:--".to_owned());
                if row.time.label() != text {
                    row.time.set_label(&text);
                }
                let tooltip = datetime
                    .format("%Y-%m-%d (%a)")
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                row.name.set_tooltip_text(Some(&tooltip));

                // Day/night marker: 06:00–18:00 counts as day.
                let hour = datetime.hour();
                let day = (6..18).contains(&hour);
                let dot = row.dot.clone();
                dot.set_draw_func(move |_, cr, width, height| {
                    let ink = Ink::new();
                    let cx = f64::from(width) / 2.0;
                    let cy = f64::from(height) / 2.0;
                    let radius = f64::from(width).min(f64::from(height)) / 2.0 - 1.0;
                    if day {
                        graphics::disc(cr, cx, cy, radius, &ink.accent());
                    } else {
                        graphics::disc(cr, cx, cy, radius, &ink.alpha(0.25));
                    }
                });
                row.dot.queue_draw();
            }
        }
    };
    refresh();
    tick_seconds(&root, 1, refresh);
    Ok(root.upcast())
}

// ---------------------------------------------------------------------------
// calendar
// ---------------------------------------------------------------------------

pub fn calendar(definition: &Plugin, _context: &WidgetContext) -> Result<gtk::Widget> {
    let week_starts_monday = match &definition.view {
        View::Calendar {
            week_starts_monday,
        } => *week_starts_monday,
        _ => unreachable!("calendar called with the wrong view"),
    };

    let now = glib::DateTime::now_local().ok();
    let year = now.as_ref().map_or(2026, |n| n.year());
    let month = now.as_ref().map_or(1, |n| n.month());
    let today = now.as_ref().map(|n| n.day_of_month());

    let title = gtk::Label::new(Some(&format!("{year} / {month}")));
    title.add_css_class("heading");
    title.set_halign(gtk::Align::Center);

    let grid = gtk::Grid::new();
    grid.set_halign(gtk::Align::Center);
    grid.set_hexpand(true);
    grid.set_column_spacing(6);
    grid.set_row_spacing(4);

    for (col, label) in weekday_names(week_starts_monday).iter().enumerate() {
        let cell = gtk::Label::new(Some(label));
        cell.add_css_class("caption");
        cell.add_css_class("dim-label");
        grid.attach(&cell, col as i32, 0, 1, 1);
    }

    let offset = week_offset(year, month, week_starts_monday);
    let days = days_in_month(year, month);
    for day in 1..=days {
        let col = (offset + i32::from(day) - 1).rem_euclid(7);
        let row = 1 + (offset + i32::from(day) - 1) / 7;
        let cell = gtk::Label::new(Some(&day.to_string()));
        if Some(day) == today {
            cell.add_css_class("accent");
            cell.add_css_class("heading");
        }
        grid.attach(&cell, col, row, 1, 1);
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_vexpand(true);
    root.set_valign(gtk::Align::Center);
    root.append(&title);
    root.append(&grid);
    Ok(root.upcast())
}

fn weekday_names(monday: bool) -> Vec<String> {
    // 2024-01-01 was a Monday; take the locale's short names from a reference week.
    let timezone = glib::TimeZone::local();
    let mut names = Vec::with_capacity(7);
    for offset in 0..7 {
        if let Some(day) = glib::DateTime::new(&timezone, 2024, 1, 1 + offset, 12, 0, 0.0).ok() {
            names.push(day.format("%a").map(|s| s.to_string()).unwrap_or_default());
        }
    }
    if !monday {
        // Reference week starts on Monday; rotate to Sunday-first.
        names.rotate_right(1);
    }
    names
}

fn week_offset(year: i32, month: i32, monday: bool) -> i32 {
    let timezone = glib::TimeZone::local();
    let Some(first) = glib::DateTime::new(&timezone, year, month, 1, 12, 0, 0.0).ok() else {
        return 0;
    };
    // `day_of_week`: 1 = Monday … 7 = Sunday.
    let dow = first.day_of_week();
    if monday {
        i32::from(dow) - 1
    } else {
        i32::from(dow) % 7
    }
}

fn days_in_month(year: i32, month: i32) -> i32 {
    let timezone = glib::TimeZone::local();
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let Some(next) = glib::DateTime::new(&timezone, y, m, 1, 0, 0, 0.0).ok() else {
        return 31;
    };
    match next.add_days(-1).ok().map(|last| last.day_of_month()) {
        Some(days) => days,
        None => 31,
    }
}

// ---------------------------------------------------------------------------
// date ("today")
// ---------------------------------------------------------------------------

pub fn date(definition: &Plugin, _context: &WidgetContext) -> Result<gtk::Widget> {
    let (show_week, show_progress) = match &definition.view {
        View::Date {
            show_week,
            show_progress,
        } => (*show_week, *show_progress),
        _ => unreachable!("date called with the wrong view"),
    };

    let now = glib::DateTime::now_local().ok();
    let big = gtk::Label::new(Some(&format_opt(now.as_ref(), "%Y-%m-%d")));
    big.add_css_class("edm-time");
    big.set_halign(gtk::Align::Center);

    let weekday = gtk::Label::new(Some(&format_opt(now.as_ref(), "%A")));
    weekday.add_css_class("title-3");
    weekday.set_halign(gtk::Align::Center);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.set_valign(gtk::Align::Center);
    root.append(&big);
    root.append(&weekday);

    if show_week {
        let week = gtk::Label::new(Some(&format_opt(now.as_ref(), "ISO 週 %V")));
        week.add_css_class("dim-label");
        week.set_halign(gtk::Align::Center);
        root.append(&week);
    }

    if show_progress {
        let (fraction, label) = now
            .as_ref()
            .map(|now| {
                let day = f64::from(now.day_of_year());
                let total = f64::from(if now.year() % 4 == 0 { 366 } else { 365 });
                (day / total, format!("{day:.0} / {total:.0} 日"))
            })
            .unwrap_or((0.0, String::new()));

        let bar = gtk::ProgressBar::new();
        bar.set_fraction(fraction.clamp(0.0, 1.0));
        bar.set_halign(gtk::Align::Fill);
        bar.add_css_class("edm-year-progress");

        let caption = gtk::Label::new(Some(&label));
        caption.add_css_class("caption");
        caption.add_css_class("dim-label");
        caption.set_halign(gtk::Align::Center);

        root.append(&bar);
        root.append(&caption);
    }

    Ok(root.upcast())
}
