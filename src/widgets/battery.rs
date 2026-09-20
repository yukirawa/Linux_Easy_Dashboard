//! Battery widget: charge ring, charge state and the remaining time.
//!
//! Desktops simply have no battery; the widget says so quietly instead of
//! looking broken.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Ink};
use crate::platform::sysfs;
use crate::platform::sysfs::BatteryStatus;
use crate::util::human_percent;

use super::{Category, DetailPage, Readout, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "battery";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "バッテリー",
    summary: "充電量と残り時間",
    icon: "battery-symbolic",
    category: Category::System,
    default_size: (300, 220),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct BatteryConfig {
    show_time: bool,
    warn_by_charge: bool,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        Self {
            show_time: true,
            warn_by_charge: true,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Gauge {
    /// 0.0..=1.0
    fraction: f64,
    charging: bool,
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: BatteryConfig = context.config();
    let gauge = Rc::new(RefCell::new(Gauge::default()));
    let ink = Ink::new();

    let empty = gtk::Label::new(Some("バッテリーがありません"));
    empty.add_css_class("dim-label");
    empty.set_valign(gtk::Align::Center);
    empty.set_vexpand(true);

    let area = gtk::DrawingArea::new();
    area.set_content_width(96);
    area.set_content_height(96);
    area.set_vexpand(true);
    area.set_hexpand(true);
    {
        let gauge = gauge.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw(
                cr,
                f64::from(width),
                f64::from(height),
                &gauge.borrow(),
                &ink,
                config,
            );
        });
    }

    let readout = Readout::new();
    let charge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    charge.set_halign(gtk::Align::Center);
    let charge_icon = gtk::Image::from_icon_name("battery-symbolic");
    charge_icon.add_css_class("edm-ink-accent");
    charge_icon.set_pixel_size(14);
    charge.append(&charge_icon);

    let side = gtk::Box::new(gtk::Orientation::Vertical, 6);
    side.set_valign(gtk::Align::Center);
    side.set_hexpand(true);
    side.append(&readout.root);
    side.append(&charge);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.set_vexpand(true);
    root.append(&area);
    root.append(&side);
    let root = ink.wrap(&root);

    super::tick_seconds(&root, 10, move || {
        let Some(battery) = sysfs::batteries().into_iter().next() else {
            empty.set_visible(true);
            area.set_visible(false);
            side.set_visible(false);
            return;
        };
        empty.set_visible(false);
        area.set_visible(true);
        side.set_visible(true);

        *gauge.borrow_mut() = Gauge {
            fraction: battery.capacity / 100.0,
            charging: battery.status.is_charging(),
        };

        let caption = match (config.show_time, battery.minutes) {
            (true, Some(minutes)) => format!(
                "{} · {} 時間 {} 分",
                battery.status.label(),
                minutes / 60,
                minutes % 60
            ),
            _ => battery.status.label().to_owned(),
        };
        readout.set(&human_percent(battery.capacity), &caption);
        charge_icon.set_visible(battery.status == BatteryStatus::Charging);
        area.queue_draw();
    });

    Ok(root.upcast())
}

fn draw(cr: &Context, width: f64, height: f64, gauge: &Gauge, ink: &Ink, config: BatteryConfig) {
    graphics::prepare(cr);
    let size = width.min(height);
    if size < 24.0 {
        return;
    }
    let thickness = (size * 0.14).max(6.0);
    let radius = size / 2.0 - thickness / 2.0 - 1.0;

    // Charging is shown with the accent colour, otherwise the usual ramp.
    let value = if gauge.charging {
        ink.accent()
    } else if config.warn_by_charge {
        ink.level(1.0 - gauge.fraction)
    } else {
        ink.accent()
    };

    graphics::ring(
        cr,
        (width / 2.0, height / 2.0),
        radius,
        thickness,
        gauge.fraction,
        &ink.alpha(0.12),
        &value,
    );
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: BatteryConfig = context.config();
    let page = DetailPage::new("バッテリー", "表示");

    match sysfs::batteries().into_iter().next() {
        Some(battery) => {
            page.fact("充電量", &format!("{:.0}%", battery.capacity));
            page.fact("状態", battery.status.label());
            if let Some(minutes) = battery.minutes {
                page.fact(
                    "残り時間",
                    &format!("{} 時間 {} 分", minutes / 60, minutes % 60),
                );
            }
            page.fact("デバイス", &battery.name);
        }
        None => page.note("/sys/class/power_supply にバッテリーが見つかりませんでした。"),
    }

    page.switch(context, "show_time", "残り時間を表示", config.show_time);
    page.switch(
        context,
        "warn_by_charge",
        "残量で色を変える",
        config.warn_by_charge,
    );
    Ok(page.finish())
}
