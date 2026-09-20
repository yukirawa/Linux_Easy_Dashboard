//! Disk throughput widget: read and write rates of one block device, with a
//! short history per direction.
//!
//! Counters come from `/proc/diskstats` and are cumulative, so the widget keeps
//! the previous sample to turn them into rates. The device defaults to
//! "whichever disk is busy", which is what a portable dashboard wants.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use log::debug;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Bounds, Ink};
use crate::platform::procfs;
use crate::util::{History, human_bytes, human_rate};

use super::{Category, DetailPage, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "diskio";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "ディスク I/O",
    summary: "読み書きの速度と推移",
    icon: "drive-harddisk-symbolic",
    category: Category::System,
    default_size: (360, 200),
    aspect: None,
    build,
    detail,
};

/// Refresh interval, kept in sync with the rate calculation.
const INTERVAL: u32 = 2;
const HISTORY: usize = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct DiskIoConfig {
    /// Device to watch. Empty means "follow the busiest disk".
    device: String,
    show_history: bool,
}

impl Default for DiskIoConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            show_history: true,
        }
    }
}

/// Drawing input: both directions over time, scaled together.
struct Plot {
    read: History,
    write: History,
    peak: f64,
}

impl Plot {
    fn new() -> Self {
        Self {
            read: History::new(HISTORY),
            write: History::new(HISTORY),
            peak: 1024.0 * 1024.0,
        }
    }

    /// Drops the history when the widget starts watching another disk, so the
    /// curve never mixes two devices.
    fn reset(&mut self) {
        self.read = History::new(HISTORY);
        self.write = History::new(HISTORY);
        self.peak = 1024.0 * 1024.0;
    }
}

/// What the sampling loop remembers between ticks.
#[derive(Default)]
struct Sampler {
    /// Cumulative counters per device, as of the previous tick.
    counters: HashMap<String, (u64, u64)>,
    /// The device being watched.
    device: String,
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: DiskIoConfig = context.config();
    let plot = Rc::new(RefCell::new(Plot::new()));
    let ink = Ink::new();

    let header = TileHeader::new(DESCRIPTOR.icon, "ディスク");

    let read = gtk::Label::new(None);
    read.add_css_class("caption");
    read.add_css_class("edm-ink-accent");
    read.set_xalign(0.0);
    read.set_hexpand(true);

    let write = gtk::Label::new(None);
    write.add_css_class("caption");
    write.add_css_class("dim-label");
    write.set_xalign(1.0);

    let rates = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    rates.append(&read);
    rates.append(&write);

    let area = gtk::DrawingArea::new();
    area.set_content_height(52);
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let plot = plot.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw(
                cr,
                f64::from(width),
                f64::from(height),
                &plot.borrow(),
                &ink,
                config.show_history,
            );
        });
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&area);
    root.append(&rates);
    let root = ink.wrap(&root);

    let sampler = Rc::new(RefCell::new(Sampler::default()));
    let plot_for_tick = plot.clone();
    super::tick_seconds(&root, INTERVAL, move || {
        let counters: HashMap<String, (u64, u64)> = procfs::disk_io()
            .into_iter()
            .map(|disk| (disk.name, (disk.read_bytes, disk.write_bytes)))
            .collect();
        if counters.is_empty() {
            debug!("/proc/diskstats からブロックデバイスを読めません");
            header.set_value("—");
            read.set_label("ディスク情報を読めません");
            write.set_label("");
            return;
        }

        let Some(device) = watch(&sampler.borrow(), &counters, &config.device) else {
            header.set_value("—");
            read.set_label(&format!("{} が見つかりません", config.device.trim()));
            write.set_label("");
            return;
        };

        let (read_rate, write_rate) = {
            let mut sampler = sampler.borrow_mut();
            let rates = sampler.counters.get(&device).and_then(|before| {
                let (read, write) = counters.get(&device)?;
                let seconds = f64::from(INTERVAL);
                Some((
                    read.saturating_sub(before.0) as f64 / seconds,
                    write.saturating_sub(before.1) as f64 / seconds,
                ))
            });

            if sampler.device != device {
                debug!("ディスク I/O: {device} を監視します");
                plot_for_tick.borrow_mut().reset();
                sampler.device = device.clone();
            }
            sampler.counters = counters;

            // The first tick has no previous counters: it only establishes the
            // baseline the rates are measured against.
            let Some(rates) = rates else {
                header.set_value(&device);
                read.set_label("計測中…");
                write.set_label("");
                return;
            };
            rates
        };

        {
            let mut plot = plot_for_tick.borrow_mut();
            plot.read.push(read_rate);
            plot.write.push(write_rate);
            plot.read.seed(2);
            plot.write.seed(2);
            let peak = plot.read.maximum().max(plot.write.maximum()).max(1024.0);
            // Grow at once, decay slowly: a stable scale is easier to read.
            plot.peak = if peak > plot.peak {
                peak
            } else {
                (plot.peak * 0.9).max(peak)
            };
        }

        header.set_value(&device);
        read.set_label(&format!("読 {}", human_rate(read_rate)));
        write.set_label(&format!("書 {}", human_rate(write_rate)));
        area.queue_draw();
    });

    Ok(root.upcast())
}

/// Which device to watch.
///
/// The configured name wins; without one the widget follows the busiest disk,
/// keeping the current choice while everything is quiet, and starting on the
/// disk with the most traffic so the first tick already says something.
fn watch(
    sampler: &Sampler,
    counters: &HashMap<String, (u64, u64)>,
    configured: &str,
) -> Option<String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return counters
            .contains_key(configured)
            .then(|| configured.to_owned());
    }
    busiest(sampler, counters)
        .or_else(|| {
            counters
                .contains_key(&sampler.device)
                .then(|| sampler.device.clone())
        })
        .or_else(|| heaviest(counters))
}

/// The device with the most traffic since boot.
fn heaviest(counters: &HashMap<String, (u64, u64)>) -> Option<String> {
    counters
        .iter()
        .max_by_key(|(_, (read, write))| read.saturating_add(*write))
        .map(|(name, _)| name.clone())
}

/// The device with the most traffic since the previous tick, if any moved.
///
/// A machine with several disks then shows the one that is actually working
/// instead of a permanently idle system disk.
fn busiest(sampler: &Sampler, counters: &HashMap<String, (u64, u64)>) -> Option<String> {
    let mut best: Option<(u64, String)> = None;
    for (name, (read, write)) in counters {
        let Some((before_read, before_write)) = sampler.counters.get(name) else {
            continue;
        };
        let delta = read
            .saturating_sub(*before_read)
            .saturating_add(write.saturating_sub(*before_write));
        if delta == 0 {
            continue;
        }
        if best.as_ref().is_none_or(|(previous, _)| delta > *previous) {
            best = Some((delta, name.clone()));
        }
    }
    best.map(|(_, name)| name)
}

fn draw(cr: &Context, width: f64, height: f64, plot: &Plot, ink: &Ink, show_history: bool) {
    graphics::prepare(cr);
    if width <= 4.0 || height <= 4.0 {
        return;
    }

    if !show_history {
        // Two quiet meters, drawn like the bars used elsewhere in the app.
        let meters = plot
            .read
            .latest()
            .unwrap_or(0.0)
            .max(plot.write.latest().unwrap_or(0.0));
        let scale = plot.peak.max(meters).max(1.0);
        let bar_height = (height / 4.0).clamp(4.0, 10.0);
        graphics::bar(
            cr,
            Bounds::new(0.0, height * 0.28, width, bar_height),
            plot.read.latest().unwrap_or(0.0) / scale,
            &ink.alpha(0.12),
            &ink.accent(),
        );
        graphics::bar(
            cr,
            Bounds::new(0.0, height * 0.66, width, bar_height),
            plot.write.latest().unwrap_or(0.0) / scale,
            &ink.alpha(0.12),
            &ink.alpha(0.45),
        );
        return;
    }

    // Reads on top, writes underneath, on one shared scale so the halves are
    // comparable.
    let half = (height - 5.0) / 2.0;
    graphics::sparkline(
        cr,
        Bounds::new(0.0, 0.0, width, half),
        &plot.read.samples(),
        Some(plot.peak),
        &ink.accent(),
    );
    graphics::sparkline(
        cr,
        Bounds::new(0.0, half + 5.0, width, half),
        &plot.write.samples(),
        Some(plot.peak),
        &ink.alpha(0.45),
    );
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: DiskIoConfig = context.config();
    let page = DetailPage::new("ブロックデバイス", "表示");

    let disks = procfs::disk_io();
    if disks.is_empty() {
        page.note("/proc/diskstats を読めませんでした。");
    }
    for disk in &disks {
        page.fact(
            &disk.name,
            &format!(
                "読 {} · 書 {}",
                human_bytes(disk.read_bytes),
                human_bytes(disk.write_bytes)
            ),
        );
    }

    page.entry(
        context,
        "device",
        "監視するデバイス (空で一番忙しいディスク)",
        &config.device,
    );
    page.switch(context, "show_history", "推移を表示", config.show_history);
    Ok(page.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busiest_device_wins() {
        let mut sampler = Sampler::default();
        sampler.counters.insert("sda".to_owned(), (100, 100));
        sampler.counters.insert("nvme0n1".to_owned(), (500, 500));

        let counters: HashMap<String, (u64, u64)> = [
            ("sda".to_owned(), (150, 100)),
            ("nvme0n1".to_owned(), (500, 500)),
        ]
        .into_iter()
        .collect();
        assert_eq!(busiest(&sampler, &counters), Some("sda".to_owned()));
    }

    #[test]
    fn a_quiet_machine_keeps_the_current_device() {
        let sampler = Sampler::default();
        let counters: HashMap<String, (u64, u64)> =
            [("sda".to_owned(), (10, 10))].into_iter().collect();
        // Nothing moved since the last tick, so nothing is "busy" — but the
        // widget still has to pick a disk instead of showing an error forever.
        assert_eq!(busiest(&sampler, &counters), None);
        assert_eq!(watch(&sampler, &counters, ""), Some("sda".to_owned()));
    }

    #[test]
    fn the_biggest_disk_is_the_first_guess() {
        let sampler = Sampler::default();
        let counters: HashMap<String, (u64, u64)> = [
            ("sda".to_owned(), (10, 10)),
            ("nvme0n1".to_owned(), (900, 100)),
        ]
        .into_iter()
        .collect();
        assert_eq!(watch(&sampler, &counters, ""), Some("nvme0n1".to_owned()));
    }

    #[test]
    fn a_configured_device_is_used_even_when_it_is_idle() {
        let sampler = Sampler::default();
        let counters: HashMap<String, (u64, u64)> = [
            ("sda".to_owned(), (10, 10)),
            ("nvme0n1".to_owned(), (900, 100)),
        ]
        .into_iter()
        .collect();
        assert_eq!(watch(&sampler, &counters, "sda"), Some("sda".to_owned()));
        // A name that is not a block device is reported instead of guessed.
        assert_eq!(watch(&sampler, &counters, "sdz"), None);
    }
}
