//! World clock: the same moment in several timezones, with a day/night hint.
//!
//! Timezones are the system's own (`/usr/share/zoneinfo` through GLib), so
//! daylight saving is handled by the platform.

use anyhow::Result;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Category, DetailPage, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "world_clock";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "世界時計",
    summary: "複数のタイムゾーンの時刻",
    icon: "globe-symbolic",
    category: Category::Time,
    default_size: (320, 240),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct WorldClockConfig {
    /// IANA timezone identifiers.
    zones: Vec<String>,
    hour24: bool,
}

impl Default for WorldClockConfig {
    fn default() -> Self {
        Self {
            zones: vec![
                "Asia/Tokyo".to_owned(),
                "Europe/London".to_owned(),
                "America/New_York".to_owned(),
                "UTC".to_owned(),
            ],
            hour24: true,
        }
    }
}

/// One row of the clock.
struct ZoneRow {
    zone: String,
    root: gtk::Box,
    name: gtk::Label,
    time: gtk::Label,
    marker: gtk::Box,
}

impl ZoneRow {
    fn new(zone: &str) -> Self {
        let name = gtk::Label::new(Some(&city_of(zone)));
        name.add_css_class("dim-label");
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let time = gtk::Label::new(None);
        time.add_css_class("edm-mono");
        time.set_xalign(1.0);

        let marker = gtk::Box::new(gtk::Orientation::Vertical, 0);
        marker.add_css_class("edm-day-marker");
        marker.set_size_request(8, 8);
        marker.set_valign(gtk::Align::Center);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.append(&marker);
        root.append(&name);
        root.append(&time);

        Self {
            zone: zone.to_owned(),
            root,
            name,
            time,
            marker,
        }
    }

    fn refresh(&self, hour24: bool) {
        let Some(datetime) = now_in(&self.zone) else {
            self.time.set_label("—");
            self.name.set_tooltip_text(None);
            self.set_day(None);
            return;
        };

        let format = if hour24 { "%H:%M" } else { "%I:%M %p" };
        let time = datetime
            .format(format)
            .map_or_else(|_| "—".to_owned(), |text| text.to_string());
        self.time.set_label(&time);

        let tooltip = datetime
            .format("%Y-%m-%d %H:%M (%a)")
            .map(|text| text.to_string())
            .unwrap_or_default();
        self.name.set_tooltip_text(Some(&tooltip));

        self.set_day(Some((7..19).contains(&datetime.hour())));
    }

    fn set_day(&self, day: Option<bool>) {
        self.marker.remove_css_class("edm-day");
        self.marker.remove_css_class("edm-night");
        match day {
            Some(true) => self.marker.add_css_class("edm-day"),
            Some(false) => self.marker.add_css_class("edm-night"),
            None => {}
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: WorldClockConfig = context.config();

    let rows: Vec<ZoneRow> = config.zones.iter().map(|zone| ZoneRow::new(zone)).collect();

    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    list.set_vexpand(true);
    list.set_valign(gtk::Align::Center);
    for row in &rows {
        list.append(&row.root);
    }

    super::tick_seconds(&list, 1, move || {
        for row in &rows {
            row.refresh(config.hour24);
        }
    });

    Ok(list.upcast())
}

fn now_in(zone: &str) -> Option<glib::DateTime> {
    let timezone = glib::TimeZone::new(Some(zone));
    glib::DateTime::now(&timezone).ok()
}

/// `Asia/Tokyo` -> `Tokyo`.
fn city_of(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: WorldClockConfig = context.config();
    let page = DetailPage::new("タイムゾーン", "表示");

    page.list(
        context,
        "zones",
        "タイムゾーン (カンマ区切り)",
        &config.zones,
    );
    page.switch(context, "hour24", "24 時間表示", config.hour24);
    page.note("例: Asia/Tokyo, Europe/London, America/New_York, UTC");

    for zone in &config.zones {
        let row = libadwaita::ActionRow::builder()
            .title(city_of(zone))
            .subtitle(zone)
            .build();
        page.fact_row(&row);
    }

    Ok(page.finish())
}
