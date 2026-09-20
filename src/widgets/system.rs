//! System monitor widget: CPU, memory, load average, uptime and disk usage,
//! read straight from `/proc` and `statvfs`.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::prelude::*;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use crate::platform::procfs;
use crate::util::{human_bytes, human_duration, human_percent};

use super::{Category, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "system";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "システム",
    summary: "CPU・メモリ・負荷・ディスクの使用状況",
    icon: "utilities-system-monitor-symbolic",
    category: Category::System,
    default_size: (360, 300),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct SystemConfig {
    show_cpu: bool,
    show_memory: bool,
    show_load: bool,
    show_disk: bool,
    /// Filesystem shown by the disk row.
    disk_path: String,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            show_cpu: true,
            show_memory: true,
            show_load: true,
            show_disk: true,
            disk_path: "/".to_owned(),
        }
    }
}

/// One `label / bar / value` line.
struct Metric {
    row: gtk::Box,
    bar: gtk::ProgressBar,
    value: gtk::Label,
}

impl Metric {
    fn new(title: &str) -> Self {
        let label = gtk::Label::new(Some(title));
        label.add_css_class("dim-label");
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);

        let value = gtk::Label::new(None);
        value.add_css_class("edm-value");
        value.set_xalign(1.0);
        value.set_halign(gtk::Align::End);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(&label);
        header.append(&value);

        let bar = gtk::ProgressBar::new();
        bar.add_css_class("edm-bar");
        bar.set_show_text(false);
        bar.set_valign(gtk::Align::Center);

        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        row.append(&header);
        row.append(&bar);
        Self { row, bar, value }
    }

    fn set(&self, value: String, fraction: f64) {
        self.value.set_label(&value);
        self.bar.set_fraction(fraction.clamp(0.0, 1.0));
    }
}

struct SystemView {
    config: SystemConfig,
    cpu: Metric,
    memory: Metric,
    detail: gtk::Label,
    previous: RefCell<Option<procfs::CpuTimes>>,
}

impl SystemView {
    fn new(config: SystemConfig) -> Rc<Self> {
        Rc::new(Self {
            config,
            cpu: Metric::new("CPU"),
            memory: Metric::new("メモリ"),
            detail: gtk::Label::new(None),
            previous: RefCell::new(None),
        })
    }

    /// The compact tile: two meters plus a load/uptime footer.
    fn body(&self) -> gtk::Widget {
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
        bx.set_valign(gtk::Align::Center);
        bx.set_vexpand(true);
        bx.set_halign(gtk::Align::Fill);

        if self.config.show_cpu {
            bx.append(&self.cpu.row);
        }
        if self.config.show_memory {
            bx.append(&self.memory.row);
        }
        if self.config.show_load || self.config.show_disk {
            self.detail.add_css_class("caption");
            self.detail.add_css_class("dim-label");
            self.detail.set_xalign(0.0);
            self.detail.set_halign(gtk::Align::Start);
            self.detail.set_wrap(true);
            bx.append(&self.detail);
        }

        bx.upcast()
    }

    fn refresh(&self) {
        let sample = procfs::cpu_times();
        if let Some(current) = sample {
            let mut previous = self.previous.borrow_mut();
            let usage = previous.and_then(|previous| current.usage_since(previous));
            *previous = Some(current);
            if let Some(usage) = usage {
                self.cpu.set(human_percent(usage), usage / 100.0);
            }
        } else if self.previous.borrow().is_none() {
            self.cpu.set("—".to_owned(), 0.0);
        }

        if let Some(memory) = procfs::memory() {
            let fraction = memory.used() as f64 / memory.total as f64;
            self.memory.set(
                format!(
                    "{} / {}",
                    human_bytes(memory.used()),
                    human_bytes(memory.total)
                ),
                fraction,
            );
        }

        let mut parts = Vec::new();
        if self.config.show_load {
            if let Some([one, five, fifteen]) = procfs::load_average() {
                parts.push(format!(
                    "負荷 {one:.2} {five:.2} {fifteen:.2} ({})",
                    procfs::cpu_count()
                ));
            }
        }
        if self.config.show_disk {
            let path = self.config.disk_path.clone();
            match procfs::disk_usage(&path) {
                Some((total, used)) => parts.push(format!(
                    "{path} {} / {}",
                    human_bytes(used),
                    human_bytes(total)
                )),
                None => parts.push(format!("{path} を読めません")),
            }
        }
        if let Some(uptime) = procfs::uptime() {
            parts.push(format!("稼働 {}", human_duration(uptime)));
        }
        self.detail.set_label(&parts.join(" · "));
    }

    /// The full picture for the detail dialog.
    fn detail_content(&self, context: &WidgetContext) -> gtk::Widget {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(12);
        content.set_margin_bottom(24);
        content.set_margin_start(12);
        content.set_margin_end(12);

        let meters = gtk::Box::new(gtk::Orientation::Vertical, 12);
        meters.add_css_class("card");
        meters.set_margin_top(18);
        meters.set_margin_bottom(18);
        meters.set_margin_start(18);
        meters.set_margin_end(18);
        meters.append(&self.cpu.row);
        meters.append(&self.memory.row);
        content.append(&meters);

        let facts = libadwaita::PreferencesGroup::new();
        facts.set_title("詳細");

        let mut rows: Vec<(&str, String)> = Vec::new();
        if let Some(model) = procfs::cpu_model() {
            rows.push(("CPU", model));
        }
        rows.push(("論理 CPU", procfs::cpu_count().to_string()));
        if let Some(kernel) = procfs::kernel_release() {
            rows.push(("カーネル", kernel));
        }
        if let Some(host) = procfs::hostname() {
            rows.push(("ホスト", host));
        }
        if let Some(uptime) = procfs::uptime() {
            rows.push(("稼働時間", human_duration(uptime)));
        }
        if let Some(memory) = procfs::memory() {
            rows.push((
                "メモリ",
                format!(
                    "{} / {}",
                    human_bytes(memory.used()),
                    human_bytes(memory.total)
                ),
            ));
            if memory.swap_total > 0 {
                rows.push((
                    "スワップ",
                    format!(
                        "{} / {}",
                        human_bytes(memory.swap_used()),
                        human_bytes(memory.swap_total)
                    ),
                ));
            }
        }
        if let Some([one, five, fifteen]) = procfs::load_average() {
            rows.push(("負荷平均", format!("{one:.2} {five:.2} {fifteen:.2}")));
        }
        for path in ["/", "/home"] {
            if let Some((total, used)) = procfs::disk_usage(path) {
                rows.push((
                    path,
                    format!("{} / {}", human_bytes(used), human_bytes(total)),
                ));
            }
        }

        for (title, subtitle) in rows {
            let row = libadwaita::ActionRow::builder()
                .title(title)
                .subtitle(subtitle)
                .build();
            facts.add(&row);
        }
        content.append(&facts);

        let settings = libadwaita::PreferencesGroup::new();
        settings.set_title("表示する項目");
        for (key, title, active) in [
            ("show_cpu", "CPU", self.config.show_cpu),
            ("show_memory", "メモリ", self.config.show_memory),
            ("show_load", "負荷平均", self.config.show_load),
            ("show_disk", "ディスク", self.config.show_disk),
        ] {
            let row = libadwaita::SwitchRow::builder()
                .title(title)
                .active(active)
                .build();
            let context = context.clone();
            row.connect_active_notify(move |row| {
                let mut patch = serde_json::Map::new();
                patch.insert(key.to_owned(), serde_json::Value::Bool(row.is_active()));
                context.update(serde_json::Value::Object(patch));
            });
            settings.add(&row);
        }
        content.append(&settings);

        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scrolled.set_child(Some(&content));
        scrolled.set_vexpand(true);
        scrolled.upcast()
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: SystemConfig = context.config();
    let view = SystemView::new(config);

    let body = view.body();
    let tick = view.clone();
    super::tick_seconds(&body, 2, move || tick.refresh());

    Ok(body)
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: SystemConfig = context.config();
    let view = SystemView::new(config);
    let content = view.detail_content(context);

    let tick = view.clone();
    super::tick_seconds(&content, 2, move || tick.refresh());

    Ok(content)
}
