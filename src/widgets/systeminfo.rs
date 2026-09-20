//! System information widget: which distribution, kernel and hardware this
//! machine is made of.
//!
//! Almost everything here is static, so the widget only re-reads on a slow tick
//! (uptime is the one value that moves). Facts come from `os-release`, `/proc`
//! and `/sys`, never from the environment.

use std::time::Duration;

use anyhow::Result;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::platform::{os, procfs};
use crate::util::{human_bytes, human_duration};

use super::{Category, DetailPage, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "systeminfo";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "システム情報",
    summary: "OS・カーネル・ハードウェア",
    icon: "computer-symbolic",
    category: Category::Info,
    default_size: (380, 240),
    aspect: None,
    build,
    detail,
};

/// Nothing here changes quickly; the uptime is the only moving part.
const INTERVAL: u32 = 30;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct InfoConfig {
    /// Show when the machine booted next to the uptime.
    show_boot_time: bool,
}

impl Default for InfoConfig {
    fn default() -> Self {
        Self {
            show_boot_time: true,
        }
    }
}

/// A key/value line. Labels share a width so the values line up.
fn line(title: &str) -> (gtk::Box, gtk::Label) {
    let label = gtk::Label::new(Some(title));
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label.set_xalign(0.0);
    label.set_width_chars(9);
    label.set_halign(gtk::Align::Start);

    let value = gtk::Label::new(None);
    value.add_css_class("caption");
    value.add_css_class("edm-mono");
    value.set_xalign(1.0);
    value.set_hexpand(true);
    value.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.append(&label);
    row.append(&value);
    (row, value)
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: InfoConfig = context.config();

    let header = TileHeader::new(DESCRIPTOR.icon, "システム");
    let (kernel_row, kernel) = line("カーネル");
    let (host_row, host) = line("ホスト");
    let (cpu_row, cpu) = line("CPU");
    let (ram_row, ram) = line("メモリ");

    let caption = gtk::Label::new(None);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 5);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&kernel_row);
    root.append(&host_row);
    root.append(&cpu_row);
    root.append(&ram_row);
    root.append(&caption);

    super::tick_seconds(&root, INTERVAL, move || {
        let release = os::release();
        let name = release
            .as_ref()
            .map_or_else(|| "—".to_owned(), |release| distro_name(release).to_owned());
        header.set_value(&name);
        kernel.set_label(&procfs::kernel_release().unwrap_or_else(|| "—".to_owned()));
        host.set_label(&procfs::hostname().unwrap_or_else(|| "—".to_owned()));
        match procfs::cpu_model() {
            Some(model) => {
                cpu.set_label(&model);
                cpu.set_tooltip_text(Some(&model));
            }
            None => cpu.set_label(&format!("{} コア", procfs::cpu_count())),
        }
        ram.set_label(
            &procfs::memory().map_or_else(|| "—".to_owned(), |memory| human_bytes(memory.total)),
        );
        caption.set_label(&uptime_text(config.show_boot_time));
    });

    Ok(root.upcast())
}

/// The distribution's own name, preferring the prettier one.
fn distro_name(release: &os::Release) -> &str {
    if release.pretty_name.is_empty() {
        &release.name
    } else {
        &release.pretty_name
    }
}

/// `稼働 3日 04:05 · 起動 09/17 03:12`.
fn uptime_text(show_boot_time: bool) -> String {
    let Some(uptime) = procfs::uptime() else {
        return "稼働時間を読めません".to_owned();
    };
    let mut text = format!("稼働 {}", human_duration(uptime));
    if show_boot_time && let Some(stamp) = boot_stamp(uptime) {
        text.push_str(&format!(" · 起動 {stamp}"));
    }
    text
}

/// When the machine booted, as `now - uptime`, in the locale's short form.
fn boot_stamp(uptime: Duration) -> Option<String> {
    let boot = boot_time(uptime)?;
    boot.format("%m/%d %H:%M").ok().map(|text| text.to_string())
}

/// The boot time itself, for the detail view.
fn boot_time(uptime: Duration) -> Option<glib::DateTime> {
    let now = glib::DateTime::now_local().ok()?;
    now.add_seconds(-(uptime.as_secs() as f64)).ok()
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: InfoConfig = context.config();
    let page = DetailPage::new("このマシン", "表示");

    match os::release() {
        Some(release) => {
            page.fact("ディストリビューション", distro_name(&release));
            if !release.id.is_empty() {
                page.fact("ID", &release.id);
            }
            page.fact(
                "バージョン",
                if release.version.is_empty() {
                    "— (ローリング)"
                } else {
                    &release.version
                },
            );
            if !release.home_url.is_empty() {
                page.fact("ホームページ", &release.home_url);
            }
        }
        None => page.note("os-release を読めませんでした。"),
    }

    page.fact(
        "カーネル",
        &procfs::kernel_release().unwrap_or_else(|| "—".to_owned()),
    );
    page.fact("アーキテクチャ", std::env::consts::ARCH);
    page.fact(
        "ホスト名",
        &procfs::hostname().unwrap_or_else(|| "—".to_owned()),
    );
    page.fact(
        "CPU",
        &procfs::cpu_model().unwrap_or_else(|| "—".to_owned()),
    );
    page.fact("論理コア", &procfs::cpu_count().to_string());
    if let Some(memory) = procfs::memory() {
        page.fact("メモリ", &human_bytes(memory.total));
    }

    match (procfs::uptime(), procfs::uptime().and_then(boot_time)) {
        (Some(uptime), Some(boot)) => {
            page.fact("稼働時間", &human_duration(uptime));
            page.fact(
                "起動時刻",
                &boot
                    .format("%Y年%m月%d日 %H:%M")
                    .map_or_else(|_| "—".to_owned(), |text| text.to_string()),
            );
        }
        (Some(uptime), None) => page.fact("稼働時間", &human_duration(uptime)),
        _ => page.note("/proc/uptime を読めませんでした。"),
    }

    page.switch(
        context,
        "show_boot_time",
        "起動時刻を表示",
        config.show_boot_time,
    );
    Ok(page.finish())
}
