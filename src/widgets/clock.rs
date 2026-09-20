//! Clock widget: local time and date, formatted through the locale aware
//! `glib::DateTime` so `TZ`, `LC_TIME` and the system timezone all just work.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::prelude::*;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Category, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "clock";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "時計",
    summary: "時刻と日付を表示します",
    icon: "preferences-system-time-symbolic",
    category: Category::Time,
    default_size: (360, 170),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ClockConfig {
    /// 24 hour clock instead of the locale's 12 hour clock.
    hour24: bool,
    show_seconds: bool,
    show_date: bool,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            hour24: true,
            show_seconds: false,
            show_date: true,
        }
    }
}

impl ClockConfig {
    fn time_format(&self) -> &'static str {
        match (self.hour24, self.show_seconds) {
            (true, true) => "%H:%M:%S",
            (true, false) => "%H:%M",
            (false, true) => "%I:%M:%S %p",
            (false, false) => "%I:%M %p",
        }
    }

    fn date_format(&self) -> &'static str {
        "%Y-%m-%d (%a)"
    }
}

/// The live view, shared between the tile and the detail dialog.
struct ClockView {
    config: Rc<RefCell<ClockConfig>>,
    time: gtk::Label,
    date: gtk::Label,
    zone: gtk::Label,
}

impl ClockView {
    fn new(config: ClockConfig) -> Rc<Self> {
        let time = gtk::Label::new(None);
        time.add_css_class("edm-time");
        time.set_xalign(0.0);
        time.set_halign(gtk::Align::Center);

        let date = gtk::Label::new(None);
        date.add_css_class("dim-label");
        date.set_halign(gtk::Align::Center);

        let zone = gtk::Label::new(None);
        zone.add_css_class("caption");
        zone.add_css_class("dim-label");
        zone.set_halign(gtk::Align::Center);

        Rc::new(Self {
            config: Rc::new(RefCell::new(config)),
            time,
            date,
            zone,
        })
    }

    /// The stack of labels, vertically centred inside the tile.
    fn body(&self) -> gtk::Widget {
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 2);
        bx.set_valign(gtk::Align::Center);
        bx.set_vexpand(true);
        bx.set_halign(gtk::Align::Fill);
        bx.append(&self.time);
        bx.append(&self.date);
        bx.upcast()
    }

    fn refresh(&self) {
        let now = glib::DateTime::now_local().ok();
        let config = self.config.borrow();

        let text = now
            .as_ref()
            .and_then(|now| now.format(config.time_format()).ok())
            .map_or_else(|| "--:--".to_owned(), |s| s.to_string());
        if self.time.label() != text {
            self.time.set_label(&text);
        }

        let date_text = if config.show_date {
            now.as_ref()
                .and_then(|now| now.format(config.date_format()).ok())
                .map_or_else(String::new, |s| s.to_string())
        } else {
            String::new()
        };
        if self.date.label() != date_text {
            self.date.set_label(&date_text);
        }

        let zone = if config.show_date {
            now.as_ref()
                .map(|now| now.timezone_abbreviation().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        if self.zone.label() != zone {
            self.zone.set_label(&zone);
        }

        self.date.set_visible(config.show_date);
        self.zone.set_visible(config.show_date);
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: ClockConfig = context.config();
    let view = ClockView::new(config);

    let body = view.body();
    let tick = view.clone();
    super::tick_seconds(&body, 1, move || tick.refresh());

    Ok(body)
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: ClockConfig = context.config();
    let view = ClockView::new(config);
    view.time.add_css_class("edm-time-large");
    view.date.add_css_class("title-3");

    let body = view.body();
    let tick = view.clone();
    super::tick_seconds(&body, 1, move || tick.refresh());

    let clock_group = gtk::Box::new(gtk::Orientation::Vertical, 4);
    clock_group.add_css_class("card");
    clock_group.set_margin_top(18);
    clock_group.set_margin_bottom(18);
    clock_group.set_margin_start(18);
    clock_group.set_margin_end(18);
    clock_group.append(&body);

    let group = libadwaita::PreferencesGroup::new();
    group.set_title("表示");

    for toggle in TOGGLES {
        let row = libadwaita::SwitchRow::builder()
            .title(toggle.title)
            .active((toggle.get)(&view.config.borrow()))
            .build();

        let view = view.clone();
        let context = context.clone();
        row.connect_active_notify(move |row| {
            let value = row.is_active();
            (toggle.set)(&mut view.config.borrow_mut(), value);
            view.refresh();
            let mut patch = serde_json::Map::new();
            patch.insert(toggle.key.to_owned(), serde_json::Value::Bool(value));
            context.update(serde_json::Value::Object(patch));
        });
        group.add(&row);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(24);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&clock_group);
    content.append(&group);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_child(Some(&content));
    scrolled.set_vexpand(true);

    Ok(scrolled.upcast())
}

/// The settings rows offered in the detail view.
#[derive(Clone, Copy)]
struct Toggle {
    key: &'static str,
    title: &'static str,
    get: fn(&ClockConfig) -> bool,
    set: fn(&mut ClockConfig, bool),
}

const TOGGLES: [Toggle; 3] = [
    Toggle {
        key: "hour24",
        title: "24 時間表示",
        get: |c| c.hour24,
        set: |c, v| c.hour24 = v,
    },
    Toggle {
        key: "show_seconds",
        title: "秒を表示",
        get: |c| c.show_seconds,
        set: |c, v| c.show_seconds = v,
    },
    Toggle {
        key: "show_date",
        title: "日付を表示",
        get: |c| c.show_date,
        set: |c, v| c.show_date = v,
    },
];
