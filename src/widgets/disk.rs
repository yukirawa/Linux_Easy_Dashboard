//! Disk widget: one ring per mounted filesystem.
//!
//! Which filesystems are shown can be pinned in the settings; by default the
//! widget picks the first few real mounts reported by `/proc/mounts`.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Ink};
use crate::platform::procfs;
use crate::util::{human_bytes, human_percent};

use super::{Category, DetailPage, Spin, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "disk";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "ディスク",
    summary: "ファイルシステムごとの使用量",
    icon: "drive-harddisk-symbolic",
    category: Category::System,
    default_size: (360, 240),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct DiskConfig {
    /// Mount points to show. Empty means "pick a few automatically".
    mounts: Vec<String>,
    max_rows: usize,
    show_free: bool,
}

impl Default for DiskConfig {
    fn default() -> Self {
        Self {
            mounts: Vec::new(),
            max_rows: 3,
            show_free: false,
        }
    }
}

/// One mount, ready to be drawn and labelled.
#[derive(Debug, Clone)]
struct Entry {
    point: String,
    fraction: f64,
    used: u64,
    total: u64,
}

impl Entry {
    fn detail(&self, show_free: bool) -> String {
        if show_free {
            format!(
                "空き {} / {}",
                human_bytes(self.total.saturating_sub(self.used)),
                human_bytes(self.total)
            )
        } else {
            format!(
                "{} / {} ({})",
                human_bytes(self.used),
                human_bytes(self.total),
                human_percent(self.fraction * 100.0)
            )
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: DiskConfig = context.config();

    let header = TileHeader::new(DESCRIPTOR.icon, "ディスク");
    let empty = gtk::Label::new(Some("マウントが見つかりません"));
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

    // Rows are rebuilt only when the set of mounts changes, so the common case
    // is a label update instead of a rebuild.
    let rows: Rc<RefCell<Vec<(Entry, Row)>>> = Rc::new(RefCell::new(Vec::new()));

    super::tick_seconds(&root, 10, move || {
        let entries = collect(&config);
        if entries.is_empty() {
            header.set_value("—");
            empty.set_visible(true);
            list.set_visible(false);
            return;
        }
        empty.set_visible(false);
        list.set_visible(true);

        let same_set = {
            let rows = rows.borrow();
            rows.len() == entries.len()
                && rows
                    .iter()
                    .zip(entries.iter())
                    .all(|((known, _), entry)| known.point == entry.point)
        };

        let mut rows = rows.borrow_mut();
        if !same_set {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            rows.clear();
            for entry in entries {
                let row = Row::new(&entry.point);
                list.append(&row.root);
                rows.push((entry, row));
            }
        }

        for (entry, row) in rows.iter() {
            row.update(entry, config.show_free);
        }

        let most_used = rows
            .iter()
            .map(|(entry, _)| entry.fraction)
            .fold(0.0, f64::max);
        header.set_value(&human_percent(most_used * 100.0));
    });

    Ok(root.upcast())
}

/// Ring + labels for one mount.
struct Row {
    root: gtk::Overlay,
    name: gtk::Label,
    detail: gtk::Label,
    area: gtk::DrawingArea,
    fraction: Rc<RefCell<f64>>,
}

impl Row {
    fn new(point: &str) -> Self {
        let fraction = Rc::new(RefCell::new(0.0));

        let ink = Ink::new();
        let area = gtk::DrawingArea::new();
        area.set_content_width(30);
        area.set_content_height(30);
        area.set_valign(gtk::Align::Center);
        {
            let fraction = fraction.clone();
            let ink = ink.clone();
            area.set_draw_func(move |_, cr, width, height| {
                draw_ring(
                    cr,
                    f64::from(width),
                    f64::from(height),
                    *fraction.borrow(),
                    &ink,
                );
            });
        }

        let name = gtk::Label::new(Some(point));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.add_css_class("edm-mono");

        let detail = gtk::Label::new(None);
        detail.add_css_class("caption");
        detail.add_css_class("dim-label");
        detail.set_xalign(0.0);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_valign(gtk::Align::Center);
        text.set_hexpand(true);
        text.append(&name);
        text.append(&detail);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        root.append(&area);
        root.append(&text);

        Self {
            root: ink.wrap(&root),
            name,
            detail,
            area,
            fraction,
        }
    }

    fn update(&self, entry: &Entry, show_free: bool) {
        self.name.set_label(&entry.point);
        let detail = entry.detail(show_free);
        self.detail.set_label(&detail);
        self.name.set_tooltip_text(Some(&detail));
        *self.fraction.borrow_mut() = entry.fraction;
        self.area.queue_draw();
    }
}

fn draw_ring(cr: &Context, width: f64, height: f64, fraction: f64, ink: &Ink) {
    graphics::prepare(cr);
    let size = width.min(height);
    if size < 10.0 {
        return;
    }
    let thickness = (size * 0.16).max(3.0);
    graphics::ring(
        cr,
        (width / 2.0, height / 2.0),
        size / 2.0 - thickness / 2.0,
        thickness,
        fraction,
        &ink.alpha(0.15),
        &ink.level(fraction),
    );
}

/// Decides which filesystems to show, honouring the pinned list.
fn collect(config: &DiskConfig) -> Vec<Entry> {
    let available = procfs::mounts();
    let points: Vec<String> = if config.mounts.is_empty() {
        let mut seen_devices: Vec<String> = Vec::new();
        available
            .iter()
            .filter(|mount| {
                let fresh = !seen_devices.contains(&mount.device);
                if fresh {
                    seen_devices.push(mount.device.clone());
                }
                fresh
            })
            .map(|mount| mount.point.clone())
            .take(config.max_rows.max(1))
            .collect()
    } else {
        config.mounts.clone()
    };

    points
        .iter()
        .filter_map(|point| {
            let (total, used) = procfs::disk_usage(point)?;
            Some(Entry {
                point: point.clone(),
                fraction: used as f64 / total as f64,
                used,
                total,
            })
        })
        .collect()
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: DiskConfig = context.config();
    let page = DetailPage::new("ファイルシステム", "表示");

    for mount in procfs::mounts() {
        if let Some((total, used)) = procfs::disk_usage(&mount.point) {
            page.fact(
                &mount.point,
                &format!(
                    "{} · {} / {} ({})",
                    mount.fstype,
                    human_bytes(used),
                    human_bytes(total),
                    human_percent(used as f64 / total as f64 * 100.0)
                ),
            );
        }
    }

    page.list(
        context,
        "mounts",
        "表示するマウント (カンマ区切り・空で自動)",
        &config.mounts,
    );
    page.spin(
        context,
        Spin::new(
            "max_rows",
            "自動選択の最大行数",
            config.max_rows as f64,
            1.0,
            8.0,
        ),
    );
    page.switch(context, "show_free", "空き容量を表示", config.show_free);
    Ok(page.finish())
}
