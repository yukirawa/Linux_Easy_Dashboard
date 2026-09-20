//! Temperature widget: one bar per hardware sensor (`/sys/class/hwmon`).
//!
//! Machines without readable sensors show a quiet note instead of an error.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Bounds, Ink};
use crate::platform::sysfs;

use super::{Category, DetailPage, Spin, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "temperature";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "温度",
    summary: "CPU や NVMe の温度センサー",
    icon: "temperature-symbolic",
    category: Category::System,
    default_size: (340, 220),
    aspect: None,
    build,
    detail,
};

/// Temperature that fills the bar completely.
const SCALE: f64 = 100.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct TemperatureConfig {
    max_rows: usize,
}

impl Default for TemperatureConfig {
    fn default() -> Self {
        Self { max_rows: 4 }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: TemperatureConfig = context.config();

    let header = TileHeader::new(DESCRIPTOR.icon, "温度");
    let empty = gtk::Label::new(Some("センサーが見つかりません"));
    empty.add_css_class("dim-label");
    empty.set_visible(false);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    list.set_vexpand(true);
    list.set_valign(gtk::Align::Center);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&list);
    root.append(&empty);

    let rows: Rc<RefCell<Vec<Row>>> = Rc::new(RefCell::new(Vec::new()));

    super::tick_seconds(&root, 3, move || {
        let sensors: Vec<sysfs::Sensor> = sysfs::temperature_sensors()
            .into_iter()
            .take(config.max_rows.max(1))
            .collect();

        if sensors.is_empty() {
            header.set_value("—");
            empty.set_visible(true);
            list.set_visible(false);
            return;
        }
        empty.set_visible(false);
        list.set_visible(true);

        let mut rows = rows.borrow_mut();
        if rows.len() != sensors.len() {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            rows.clear();
            for _ in &sensors {
                let row = Row::new();
                list.append(&row.root);
                rows.push(row);
            }
        }

        for (row, sensor) in rows.iter().zip(sensors.iter()) {
            row.update(sensor);
        }

        let hottest = sensors
            .iter()
            .map(|sensor| sensor.celsius)
            .fold(0.0, f64::max);
        header.set_value(&format!("{hottest:.0}°C"));
    });

    Ok(root.upcast())
}

/// One sensor: label, bar, value.
struct Row {
    root: gtk::Overlay,
    name: gtk::Label,
    value: gtk::Label,
    area: gtk::DrawingArea,
    reading: Rc<RefCell<Reading>>,
}

#[derive(Default, Clone, Copy)]
struct Reading {
    fraction: f64,
}

impl Row {
    fn new() -> Self {
        let name = gtk::Label::new(None);
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let value = gtk::Label::new(None);
        value.add_css_class("edm-mono");
        value.set_xalign(1.0);

        let reading = Rc::new(RefCell::new(Reading::default()));
        let ink = Ink::new();
        let area = gtk::DrawingArea::new();
        area.set_content_height(6);
        area.set_hexpand(true);
        area.set_valign(gtk::Align::Center);
        {
            let reading = reading.clone();
            let ink = ink.clone();
            area.set_draw_func(move |_, cr, width, height| {
                graphics::prepare(cr);
                let reading = *reading.borrow();
                graphics::bar(
                    cr,
                    Bounds::new(0.0, f64::from(height) / 2.0 - 3.0, f64::from(width), 6.0),
                    reading.fraction,
                    &ink.alpha(0.12),
                    &ink.level(reading.fraction),
                );
            });
        }

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.append(&name);
        top.append(&value);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
        root.append(&top);
        root.append(&area);

        Self {
            root: ink.wrap(&root),
            name,
            value,
            area,
            reading,
        }
    }

    fn update(&self, sensor: &sysfs::Sensor) {
        let label = if sensor.label == sensor.chip {
            sensor.chip.clone()
        } else {
            format!("{} ({})", sensor.label, sensor.chip)
        };
        self.name.set_label(&label);
        self.value.set_label(&format!("{:.0}°C", sensor.celsius));
        *self.reading.borrow_mut() = Reading {
            fraction: sensor.celsius / SCALE,
        };
        self.area.queue_draw();
    }
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: TemperatureConfig = context.config();
    let page = DetailPage::new("センサー", "表示");

    let sensors = sysfs::temperature_sensors();
    if sensors.is_empty() {
        page.note("/sys/class/hwmon に読み取れる温度センサーがありませんでした。");
    }
    for sensor in &sensors {
        page.fact(
            &sensor.label,
            &format!("{:.1}°C · {}", sensor.celsius, sensor.chip),
        );
    }

    page.spin(
        context,
        Spin::new(
            "max_rows",
            "表示するセンサー数",
            config.max_rows as f64,
            1.0,
            12.0,
        ),
    );
    Ok(page.finish())
}
