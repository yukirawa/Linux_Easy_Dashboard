//! `system` view family. See `src/views/mod.rs`.
//!
//! Self-driven monitors: each renderer samples procfs/sysfs on its own timer and
//! draws the tile body the way the native widgets did.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use log::debug;

use crate::graphics::{self, Bounds, Ink};
use crate::platform::procfs;
use crate::platform::procfs::{CpuTimes, NetCounters};
use crate::platform::sysfs;
use crate::platform::sysfs::BatteryStatus;
use crate::plugin::{Plugin, View};
use crate::util::{History, human_bytes, human_duration, human_percent, human_rate};
use crate::widgets::{Readout, TileHeader, WidgetContext, tick_seconds};

// ---------------------------------------------------------------------------
// cpu
// ---------------------------------------------------------------------------

/// Samples kept for the sparkline (one per second).
const CPU_HISTORY: usize = 90;

#[derive(Debug, Clone, Copy)]
struct CpuOptions {
    per_core: bool,
    show_history: bool,
}

/// Everything the drawing closure needs. Holds no widgets, so it cannot keep a
/// removed tile alive.
struct CpuPlot {
    history: History,
    cores: Vec<f64>,
}

impl CpuPlot {
    fn new() -> Self {
        Self {
            history: History::new(CPU_HISTORY),
            cores: Vec::new(),
        }
    }
}

pub fn cpu(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::Cpu {
            per_core,
            show_history,
        } => CpuOptions {
            per_core: *per_core,
            show_history: *show_history,
        },
        _ => unreachable!("cpu called with the wrong view"),
    };
    let plot = Rc::new(RefCell::new(CpuPlot::new()));
    let ink = Ink::new();

    let header = TileHeader::new(&definition.icon, "CPU");

    let area = gtk::DrawingArea::new();
    area.set_content_height(70);
    area.set_vexpand(true);
    area.set_hexpand(true);
    {
        let plot = plot.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw_cpu(
                cr,
                f64::from(width),
                f64::from(height),
                &plot.borrow(),
                &ink,
                config,
            );
        });
    }

    let caption = gtk::Label::new(None);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&area);
    root.append(&caption);
    let root = ink.wrap(&root);

    let previous: Rc<RefCell<Option<Vec<CpuTimes>>>> = Rc::new(RefCell::new(None));
    tick_seconds(&root, 1, move || {
        let samples = procfs::cpu_times_per_core();
        if samples.len() < 2 {
            header.set_value("—");
            caption.set_label("CPU 情報を読めません");
            return;
        }

        let usages = {
            let mut older = previous.borrow_mut();
            let usages = older
                .as_ref()
                .filter(|older| older.len() == samples.len())
                .map(|older| {
                    samples
                        .iter()
                        .zip(older.iter())
                        .filter_map(|(now, before)| now.usage_since(*before))
                        .collect::<Vec<f64>>()
                })
                .unwrap_or_default();
            *older = Some(samples);
            usages
        };

        let Some(total) = usages.first().copied() else {
            return;
        };

        let (cores, busiest, average) = {
            let mut plot = plot.borrow_mut();
            plot.history.push(total);
            plot.history.seed(2);
            plot.cores = usages[1..].to_vec();
            let busiest = plot.cores.iter().copied().fold(0.0, f64::max);
            (plot.cores.len(), busiest, plot.history.average())
        };

        header.set_value(&human_percent(total));
        if config.per_core && cores > 0 {
            caption.set_label(&format!("{cores} コア · 最大 {}", human_percent(busiest)));
        } else {
            caption.set_label(&format!("平均 {}", human_percent(average)));
        }
        area.queue_draw();
    });

    Ok(root.upcast())
}

fn draw_cpu(
    cr: &Context,
    width: f64,
    height: f64,
    plot: &CpuPlot,
    ink: &Ink,
    config: CpuOptions,
) {
    graphics::prepare(cr);
    if width <= 4.0 || height <= 4.0 {
        return;
    }

    let history = plot.history.samples();
    let usage = plot.history.latest().unwrap_or(0.0) / 100.0;

    if config.per_core && !plot.cores.is_empty() {
        // Cores on top, the history squeezed underneath.
        let columns_height = (height * 0.62).max(8.0);
        let colors: Vec<gtk::gdk::RGBA> = plot
            .cores
            .iter()
            .map(|core| ink.level(core / 100.0))
            .collect();
        graphics::columns(
            cr,
            Bounds::new(0.0, 0.0, width, columns_height),
            &plot.cores,
            100.0,
            2.0,
            &ink.alpha(0.1),
            &colors,
        );
        if config.show_history {
            graphics::sparkline(
                cr,
                Bounds::new(
                    0.0,
                    columns_height + 6.0,
                    width,
                    height - columns_height - 6.0,
                ),
                &history,
                Some(100.0),
                &ink.alpha(0.5),
            );
        }
    } else if config.show_history {
        graphics::sparkline(
            cr,
            Bounds::new(0.0, 0.0, width, height),
            &history,
            Some(100.0),
            &ink.level(usage),
        );
    } else {
        graphics::bar(
            cr,
            Bounds::new(0.0, height / 2.0 - 4.0, width, 8.0),
            usage,
            &ink.alpha(0.12),
            &ink.level(usage),
        );
    }
}

// ---------------------------------------------------------------------------
// cores
// ---------------------------------------------------------------------------

/// How long between samples. Load is measured over this window.
const CORES_INTERVAL: u32 = 1;

/// Cells beyond this are not drawn: they would be a pixel each. The caption
/// says how many are missing instead of pretending the machine is smaller.
const MAX_CELLS: usize = 128;

#[derive(Debug, Clone, Copy)]
struct CoresOptions {
    /// Fixed column count, or 0 to fit the tile's shape.
    columns: usize,
    /// Colour by load (accent, then warning, then error).
    warn_by_usage: bool,
}

pub fn cores(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::Cores {
            columns,
            warn_by_usage,
        } => CoresOptions {
            columns: *columns,
            warn_by_usage: *warn_by_usage,
        },
        _ => unreachable!("cores called with the wrong view"),
    };
    let usage: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let ink = Ink::new();

    let header = TileHeader::new(&definition.icon, "コア");

    let area = gtk::DrawingArea::new();
    area.set_content_height(80);
    area.set_vexpand(true);
    area.set_hexpand(true);
    {
        let usage = usage.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw_cores(
                cr,
                f64::from(width),
                f64::from(height),
                &usage.borrow(),
                &ink,
                config,
            );
        });
    }

    let caption = gtk::Label::new(None);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&area);
    root.append(&caption);
    let root = ink.wrap(&root);

    let previous: Rc<RefCell<Option<Vec<CpuTimes>>>> = Rc::new(RefCell::new(None));
    let usage_for_tick = usage.clone();
    tick_seconds(&root, CORES_INTERVAL, move || {
        let samples = procfs::cpu_times_per_core();
        if samples.len() < 2 {
            header.set_value("—");
            caption.set_label("コア情報を読めません");
            return;
        }

        // Index 0 of `/proc/stat` is the whole machine, the rest are the cores.
        let fresh = {
            let mut older = previous.borrow_mut();
            let fresh = older
                .as_ref()
                .filter(|older| older.len() == samples.len())
                .map(|older| {
                    samples
                        .iter()
                        .zip(older.iter())
                        .filter_map(|(now, before)| now.usage_since(*before))
                        .collect::<Vec<f64>>()
                })
                .unwrap_or_default();
            *older = Some(samples);
            fresh
        };

        let Some((total, cores)) = fresh.split_first() else {
            return;
        };
        let busiest = cores.iter().copied().fold(0.0, f64::max);
        header.set_value(&human_percent(*total));
        if cores.len() > MAX_CELLS {
            caption.set_label(&format!(
                "{MAX_CELLS} / {} コア · 最大 {}",
                cores.len(),
                human_percent(busiest)
            ));
        } else {
            caption.set_label(&format!(
                "{} コア · 最大 {}",
                cores.len(),
                human_percent(busiest)
            ));
        }

        *usage_for_tick.borrow_mut() = core_fractions(cores);
        area.queue_draw();
    });

    Ok(root.upcast())
}

fn draw_cores(
    cr: &Context,
    width: f64,
    height: f64,
    usage: &[f64],
    ink: &Ink,
    config: CoresOptions,
) {
    graphics::prepare(cr);
    if usage.is_empty() || width < 12.0 || height < 12.0 {
        return;
    }

    let count = usage.len();
    let columns = core_columns(count, config.columns, width / height);
    let rows = count.div_ceil(columns);
    let gap = (width.min(height) / (columns.max(rows) as f64 + 1.0) * 0.18).clamp(2.0, 5.0);
    let cell_width = (width - gap * (columns - 1) as f64) / columns as f64;
    let cell_height = (height - gap * (rows - 1) as f64) / rows as f64;
    if cell_width < 4.0 || cell_height < 4.0 {
        return;
    }
    let radius = (cell_width.min(cell_height) * 0.28).clamp(2.0, 8.0);

    for (index, fraction) in usage.iter().enumerate() {
        let fraction = fraction.clamp(0.0, 1.0);
        let row = index / columns;
        let column = index % columns;
        // A short last row is centred, so the grid looks deliberate.
        let in_row = (count - row * columns).min(columns);
        let row_width = in_row as f64 * cell_width + (in_row - 1) as f64 * gap;
        let x = (width - row_width) / 2.0 + column as f64 * (cell_width + gap);
        let y = row as f64 * (cell_height + gap);

        let mut color = if config.warn_by_usage {
            ink.level(fraction)
        } else {
            ink.accent()
        };
        // Idle cores fade into the tile instead of shouting.
        color.set_alpha((0.14 + 0.86 * fraction) as f32);
        graphics::fill_rounded(cr, x, y, cell_width, cell_height, radius, &color);
    }
}

/// `/proc/stat` reports percentages; the heatmap wants 0.0..=1.0.
fn core_fractions(percents: &[f64]) -> Vec<f64> {
    percents
        .iter()
        .take(MAX_CELLS)
        .map(|percent| (percent / 100.0).clamp(0.0, 1.0))
        .collect()
}

/// Number of columns for `count` cells in a `aspect` (width/height) box.
fn core_columns(count: usize, configured: usize, aspect: f64) -> usize {
    let count = count.max(1);
    if configured > 0 {
        return configured.min(count);
    }
    // Cells as square as the box allows.
    ((count as f64 * aspect.clamp(0.25, 4.0)).sqrt().ceil() as usize).clamp(1, count)
}

// ---------------------------------------------------------------------------
// memory
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct MemoryOptions {
    /// Show the swap line under the readout.
    show_swap: bool,
    /// Colour the ring by usage (accent, then warning, then error).
    warn_by_usage: bool,
}

/// What the drawing closure needs, without holding on to any widget.
#[derive(Default)]
struct MemoryGauge {
    fraction: f64,
}

pub fn memory(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::Memory {
            show_swap,
            warn_by_usage,
        } => MemoryOptions {
            show_swap: *show_swap,
            warn_by_usage: *warn_by_usage,
        },
        _ => unreachable!("memory called with the wrong view"),
    };

    let gauge = Rc::new(RefCell::new(MemoryGauge::default()));
    let ink = Ink::new();

    let area = gtk::DrawingArea::new();
    area.set_content_width(96);
    area.set_content_height(96);
    area.set_vexpand(true);
    area.set_hexpand(true);
    {
        let gauge = gauge.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw_memory(
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
    let swap = gtk::Label::new(None);
    swap.add_css_class("caption");
    swap.add_css_class("dim-label");
    swap.set_visible(config.show_swap);

    let side = gtk::Box::new(gtk::Orientation::Vertical, 4);
    side.set_valign(gtk::Align::Center);
    side.set_hexpand(true);
    side.append(&readout.root);
    side.append(&swap);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.set_vexpand(true);
    root.append(&area);
    root.append(&side);
    let root = ink.wrap(&root);

    let gauge = gauge.clone();
    tick_seconds(&root, 2, move || {
        let Some(memory) = procfs::memory() else {
            readout.set("—", "メモリ情報を読めません");
            swap.set_visible(false);
            return;
        };

        let fraction = memory.used() as f64 / memory.total as f64;
        gauge.borrow_mut().fraction = fraction;
        readout.set(
            &human_percent(fraction * 100.0),
            &format!(
                "{} / {}",
                human_bytes(memory.used()),
                human_bytes(memory.total)
            ),
        );

        if config.show_swap {
            swap.set_visible(true);
            if memory.swap_total == 0 {
                swap.set_label("Swap なし");
            } else {
                swap.set_label(&format!(
                    "Swap {} / {}",
                    human_bytes(memory.swap_used()),
                    human_bytes(memory.swap_total)
                ));
            }
        } else {
            swap.set_visible(false);
        }
    });

    Ok(root.upcast())
}

fn draw_memory(
    cr: &Context,
    width: f64,
    height: f64,
    gauge: &MemoryGauge,
    ink: &Ink,
    config: MemoryOptions,
) {
    graphics::prepare(cr);
    let size = width.min(height);
    if size < 24.0 {
        return;
    }
    let thickness = (size * 0.14).max(6.0);
    let radius = size / 2.0 - thickness / 2.0 - 1.0;
    let value = if config.warn_by_usage {
        ink.level(gauge.fraction)
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

// ---------------------------------------------------------------------------
// disk
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct DiskOptions {
    /// Mount points to show. Empty tracks the root filesystem.
    max_rows: usize,
    show_free: bool,
}

/// One mount, ready to be drawn and labelled.
#[derive(Debug, Clone)]
struct DiskEntry {
    point: String,
    fraction: f64,
    used: u64,
    total: u64,
}

impl DiskEntry {
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

pub fn disk(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let (mounts, config) = match &definition.view {
        View::Disk {
            mounts,
            max_rows,
            show_free,
        } => (
            super::csv(mounts),
            DiskOptions {
                max_rows: *max_rows,
                show_free: *show_free,
            },
        ),
        _ => unreachable!("disk called with the wrong view"),
    };

    let header = TileHeader::new(&definition.icon, "ディスク");
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
    let rows: Rc<RefCell<Vec<(DiskEntry, DiskRow)>>> = Rc::new(RefCell::new(Vec::new()));

    tick_seconds(&root, 10, move || {
        let entries = collect_disks(&mounts, config.max_rows);
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
                let row = DiskRow::new(&entry.point);
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
struct DiskRow {
    root: gtk::Overlay,
    name: gtk::Label,
    detail: gtk::Label,
    area: gtk::DrawingArea,
    fraction: Rc<RefCell<f64>>,
}

impl DiskRow {
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
                draw_disk_ring(
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

    fn update(&self, entry: &DiskEntry, show_free: bool) {
        self.name.set_label(&entry.point);
        let detail = entry.detail(show_free);
        self.detail.set_label(&detail);
        self.name.set_tooltip_text(Some(&detail));
        *self.fraction.borrow_mut() = entry.fraction;
        self.area.queue_draw();
    }
}

fn draw_disk_ring(cr: &Context, width: f64, height: f64, fraction: f64, ink: &Ink) {
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

/// Decides which filesystems to show: the configured list, or the root
/// filesystem when there is none, cut to `max_rows`.
fn collect_disks(mounts: &[String], max_rows: usize) -> Vec<DiskEntry> {
    let points: Vec<String> = if mounts.is_empty() {
        vec!["/".to_owned()]
    } else {
        mounts.to_vec()
    };

    points
        .iter()
        .take(max_rows.max(1))
        .filter_map(|point| {
            let (total, used) = procfs::disk_usage(point)?;
            Some(DiskEntry {
                point: point.clone(),
                fraction: used as f64 / total as f64,
                used,
                total,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// diskio
// ---------------------------------------------------------------------------

/// Refresh interval, kept in sync with the rate calculation.
const DISKIO_INTERVAL: u32 = 2;
const DISKIO_HISTORY: usize = 60;

#[derive(Debug, Clone)]
struct DiskIoOptions {
    /// Device to watch. Empty means "follow the busiest disk".
    device: String,
    show_history: bool,
}

/// Drawing input: both directions over time, scaled together.
struct DiskIoPlot {
    read: History,
    write: History,
    peak: f64,
}

impl DiskIoPlot {
    fn new() -> Self {
        Self {
            read: History::new(DISKIO_HISTORY),
            write: History::new(DISKIO_HISTORY),
            peak: 1024.0 * 1024.0,
        }
    }

    /// Drops the history when the tile starts watching another disk, so the
    /// curve never mixes two devices.
    fn reset(&mut self) {
        self.read = History::new(DISKIO_HISTORY);
        self.write = History::new(DISKIO_HISTORY);
        self.peak = 1024.0 * 1024.0;
    }
}

/// What the sampling loop remembers between ticks.
#[derive(Default)]
struct DiskIoSampler {
    /// Cumulative counters per device, as of the previous tick.
    counters: HashMap<String, (u64, u64)>,
    /// The device being watched.
    device: String,
}

pub fn diskio(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::DiskIo { device, show_history } => DiskIoOptions {
            device: device.clone(),
            show_history: *show_history,
        },
        _ => unreachable!("diskio called with the wrong view"),
    };
    let plot = Rc::new(RefCell::new(DiskIoPlot::new()));
    let ink = Ink::new();

    let header = TileHeader::new(&definition.icon, "ディスク");

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
            draw_diskio(
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

    let sampler = Rc::new(RefCell::new(DiskIoSampler::default()));
    let plot_for_tick = plot.clone();
    tick_seconds(&root, DISKIO_INTERVAL, move || {
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

        let Some(device) = watch_device(&sampler.borrow(), &counters, &config.device) else {
            header.set_value("—");
            read.set_label(&format!("{} が見つかりません", config.device.trim()));
            write.set_label("");
            return;
        };

        let (read_rate, write_rate) = {
            let mut sampler = sampler.borrow_mut();
            let rates = sampler.counters.get(&device).and_then(|before| {
                let (read, write) = counters.get(&device)?;
                let seconds = f64::from(DISKIO_INTERVAL);
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
/// The configured name wins; without one the tile follows the busiest disk,
/// keeping the current choice while everything is quiet, and starting on the
/// disk with the most traffic so the first tick already says something.
fn watch_device(
    sampler: &DiskIoSampler,
    counters: &HashMap<String, (u64, u64)>,
    configured: &str,
) -> Option<String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return counters
            .contains_key(configured)
            .then(|| configured.to_owned());
    }
    busiest_device(sampler, counters)
        .or_else(|| {
            counters
                .contains_key(&sampler.device)
                .then(|| sampler.device.clone())
        })
        .or_else(|| heaviest_device(counters))
}

/// The device with the most traffic since boot.
fn heaviest_device(counters: &HashMap<String, (u64, u64)>) -> Option<String> {
    counters
        .iter()
        .max_by_key(|(_, (read, write))| read.saturating_add(*write))
        .map(|(name, _)| name.clone())
}

/// The device with the most traffic since the previous tick, if any moved.
///
/// A machine with several disks then shows the one that is actually working
/// instead of a permanently idle system disk.
fn busiest_device(sampler: &DiskIoSampler, counters: &HashMap<String, (u64, u64)>) -> Option<String> {
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

fn draw_diskio(
    cr: &Context,
    width: f64,
    height: f64,
    plot: &DiskIoPlot,
    ink: &Ink,
    show_history: bool,
) {
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

// ---------------------------------------------------------------------------
// network
// ---------------------------------------------------------------------------

/// Refresh interval, kept in sync with the rate calculation.
const NETWORK_INTERVAL: u32 = 2;
const NETWORK_HISTORY: usize = 60;

#[derive(Debug, Clone)]
struct NetworkOptions {
    /// Interface to watch. Empty means "follow the default route".
    interface: String,
    show_history: bool,
}

/// Drawing input: both directions, scaled together so the curves are
/// comparable.
struct NetworkPlot {
    down: History,
    up: History,
    peak: f64,
}

impl NetworkPlot {
    fn new() -> Self {
        Self {
            down: History::new(NETWORK_HISTORY),
            up: History::new(NETWORK_HISTORY),
            peak: 1.0,
        }
    }
}

pub fn network(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::Network {
            interface,
            show_history,
        } => NetworkOptions {
            interface: interface.clone(),
            show_history: *show_history,
        },
        _ => unreachable!("network called with the wrong view"),
    };
    let plot = Rc::new(RefCell::new(NetworkPlot::new()));
    let ink = Ink::new();

    let header = TileHeader::new(&definition.icon, "ネットワーク");
    let down = gtk::Label::new(None);
    down.add_css_class("caption");
    down.add_css_class("edm-ink-accent");
    down.set_xalign(0.0);
    down.set_hexpand(true);

    let up = gtk::Label::new(None);
    up.add_css_class("caption");
    up.add_css_class("dim-label");
    up.set_xalign(1.0);

    let rates = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    rates.append(&down);
    rates.append(&up);

    let area = gtk::DrawingArea::new();
    area.set_content_height(44);
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let plot = plot.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw_network(cr, f64::from(width), f64::from(height), &plot.borrow(), &ink);
        });
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);
    if config.show_history {
        root.append(&area);
    }
    root.append(&rates);
    let root = ink.wrap(&root);

    // The previous counters, plus whether the first sample is still pending.
    let previous: Rc<RefCell<Option<NetCounters>>> = Rc::new(RefCell::new(None));
    let plot_for_tick = plot.clone();
    tick_seconds(&root, NETWORK_INTERVAL, move || {
        let Some((name, counters)) = current_interface(&config) else {
            header.set_value("—");
            down.set_label("インターフェースが見つかりません");
            up.set_label("");
            return;
        };

        let rates = {
            let mut previous = previous.borrow_mut();
            let rates =
                previous.map(|before| counters.rates_since(before, f64::from(NETWORK_INTERVAL)));
            *previous = Some(counters);
            rates
        };

        header.set_value(&name);
        let Some((down_rate, up_rate)) = rates else {
            return;
        };

        {
            let mut plot = plot_for_tick.borrow_mut();
            plot.down.push(down_rate);
            plot.up.push(up_rate);
            plot.down.seed(2);
            plot.up.seed(2);
            let peak = plot.down.maximum().max(plot.up.maximum()).max(1024.0);
            // Keep the scale from jumping around: grow immediately, decay
            // slowly.
            plot.peak = if peak > plot.peak {
                peak
            } else {
                (plot.peak * 0.9).max(peak)
            };
        }

        down.set_label(&format!("↓ {}", human_rate(down_rate)));
        up.set_label(&format!("↑ {}", human_rate(up_rate)));
        area.queue_draw();
    });

    Ok(root.upcast())
}

/// The interface to watch and its current counters.
fn current_interface(config: &NetworkOptions) -> Option<(String, NetCounters)> {
    let counters = procfs::network_counters();
    let wanted = if config.interface.trim().is_empty() {
        procfs::default_interface()
    } else {
        Some(config.interface.trim().to_owned())
    };
    let name = wanted?;
    let entry = counters.into_iter().find(|(name_, _)| *name_ == name)?;
    Some(entry)
}

fn draw_network(cr: &Context, width: f64, height: f64, plot: &NetworkPlot, ink: &Ink) {
    graphics::prepare(cr);
    if width <= 4.0 || height <= 4.0 {
        return;
    }
    graphics::sparkline(
        cr,
        Bounds::new(0.0, 0.0, width, height),
        &plot.down.samples(),
        Some(plot.peak),
        &ink.accent(),
    );
    graphics::sparkline(
        cr,
        Bounds::new(0.0, 0.0, width, height),
        &plot.up.samples(),
        Some(plot.peak),
        &ink.alpha(0.45),
    );
}

// ---------------------------------------------------------------------------
// load
// ---------------------------------------------------------------------------

const LOAD_HISTORY: usize = 90;

#[derive(Debug, Clone, Copy)]
struct LoadOptions {
    show_history: bool,
}

struct LoadPlot {
    history: History,
    /// Load at which every core is busy, used as the top of the scale.
    scale: f64,
}

impl LoadPlot {
    fn new() -> Self {
        Self {
            history: History::new(LOAD_HISTORY),
            scale: procfs::cpu_count().max(1) as f64,
        }
    }
}

pub fn load(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::Load { show_history } => LoadOptions {
            show_history: *show_history,
        },
        _ => unreachable!("load called with the wrong view"),
    };
    let plot = Rc::new(RefCell::new(LoadPlot::new()));
    let ink = Ink::new();

    let readout = Readout::new();
    let caption = gtk::Label::new(None);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let area = gtk::DrawingArea::new();
    area.set_content_height(36);
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let plot = plot.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw_load(
                cr,
                f64::from(width),
                f64::from(height),
                &plot.borrow(),
                &ink,
            );
        });
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.set_vexpand(true);
    root.append(&readout.root);
    if config.show_history {
        root.append(&area);
    }
    root.append(&caption);
    let root = ink.wrap(&root);

    let plot_for_tick = plot.clone();
    tick_seconds(&root, 2, move || {
        let Some([one, five, fifteen]) = procfs::load_average() else {
            readout.set("—", "負荷平均を読めません");
            caption.set_label("");
            return;
        };

        let cores = procfs::cpu_count();
        {
            let mut plot = plot_for_tick.borrow_mut();
            plot.scale = cores.max(1) as f64;
            plot.history.push(one);
            plot.history.seed(2);
        }

        readout.set(
            &format!("{one:.2}"),
            &format!(
                "{cores} コア · 1 コアあたり {}",
                human_percent(one / cores.max(1) as f64 * 100.0)
            ),
        );
        caption.set_label(&format!("5分 {five:.2} · 15分 {fifteen:.2}"));
        area.queue_draw();
    });

    Ok(root.upcast())
}

fn draw_load(cr: &Context, width: f64, height: f64, plot: &LoadPlot, ink: &Ink) {
    graphics::prepare(cr);
    if width <= 4.0 || height <= 4.0 {
        return;
    }
    let samples = plot.history.samples();
    let load = plot.history.latest().unwrap_or(0.0);

    // The track marks the "all cores busy" line, and the curve is clipped to it
    // so an overloaded machine is obvious at a glance.
    graphics::line(cr, (0.0, 0.5), (width, 0.5), height, &ink.alpha(0.08));
    graphics::sparkline(
        cr,
        Bounds::new(0.0, 0.0, width, height),
        &samples,
        Some(plot.scale.max(1.0)),
        &ink.level(load / plot.scale.max(1.0)),
    );
}

// ---------------------------------------------------------------------------
// battery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct BatteryOptions {
    show_time: bool,
    warn_by_charge: bool,
}

#[derive(Default, Clone, Copy)]
struct BatteryGauge {
    /// 0.0..=1.0
    fraction: f64,
    charging: bool,
}

pub fn battery(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::Battery {
            show_time,
            warn_by_charge,
        } => BatteryOptions {
            show_time: *show_time,
            warn_by_charge: *warn_by_charge,
        },
        _ => unreachable!("battery called with the wrong view"),
    };
    let gauge = Rc::new(RefCell::new(BatteryGauge::default()));
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
            draw_battery(
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

    tick_seconds(&root, 10, move || {
        let Some(battery) = sysfs::batteries().into_iter().next() else {
            empty.set_visible(true);
            area.set_visible(false);
            side.set_visible(false);
            return;
        };
        empty.set_visible(false);
        area.set_visible(true);
        side.set_visible(true);

        *gauge.borrow_mut() = BatteryGauge {
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

fn draw_battery(
    cr: &Context,
    width: f64,
    height: f64,
    gauge: &BatteryGauge,
    ink: &Ink,
    config: BatteryOptions,
) {
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

// ---------------------------------------------------------------------------
// processes
// ---------------------------------------------------------------------------

/// How often the list is rebuilt. Reading every `/proc/<pid>` is heavier than
/// the other widgets, so this one is slower than the rest.
const PROCESSES_INTERVAL: u32 = 3;

/// Rows beyond this are not drawn: they would not fit a tile.
const MAX_PROCESS_ROWS: usize = 12;

#[derive(Debug, Clone, Copy)]
struct ProcessOptions {
    /// Rows in the tile.
    count: usize,
    /// Rank by CPU share instead of memory.
    by_cpu: bool,
}

/// A process together with the load measured since the previous sample.
#[derive(Debug, Clone)]
struct ProcessEntry {
    pid: i32,
    name: String,
    rss: u64,
    /// Share of the whole machine's CPU since the last sample, when known.
    cpu: Option<f64>,
}

/// The cumulative counters of one sample: total CPU ticks, then per process.
type ProcessSample = (u64, HashMap<i32, u64>);

/// Reads every process and pairs it with the load since `previous`.
fn read_processes(previous: Option<&ProcessSample>) -> (Vec<ProcessEntry>, ProcessSample) {
    let processes = procfs::processes();
    let total = procfs::cpu_times().map_or(0, |times| times.total);
    let elapsed = previous.map(|(before, _)| total.saturating_sub(*before));

    let entries = processes
        .iter()
        .map(|process| ProcessEntry {
            pid: process.pid,
            name: process.name.clone(),
            rss: process.rss,
            cpu: process_share(process, previous, elapsed),
        })
        .collect();
    let ticks = processes
        .iter()
        .map(|process| (process.pid, process.cpu_ticks))
        .collect();
    (entries, (total, ticks))
}

/// The part of the machine's CPU time one process used since the last sample.
fn process_share(
    process: &procfs::Process,
    previous: Option<&ProcessSample>,
    elapsed_ticks: Option<u64>,
) -> Option<f64> {
    let elapsed = elapsed_ticks.filter(|ticks| *ticks > 0)?;
    let before = previous?.1.get(&process.pid)?;
    let ticks = process.cpu_ticks.saturating_sub(*before);
    Some((ticks as f64 / elapsed as f64).clamp(0.0, 1.0))
}

/// Heaviest first. Processes without a CPU share sort as if they used nothing,
/// which is what makes the first sample fall back to memory.
fn rank_processes(mut entries: Vec<ProcessEntry>, by_cpu: bool) -> Vec<ProcessEntry> {
    entries.sort_by(|a, b| {
        let key = |entry: &ProcessEntry| {
            if by_cpu {
                entry.cpu.unwrap_or(0.0)
            } else {
                entry.rss as f64
            }
        };
        key(b)
            .total_cmp(&key(a))
            .then_with(|| b.rss.cmp(&a.rss))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    entries
}

/// One ranked line. Built once and updated in place, so a tick never churns the
/// widget tree.
struct ProcessRow {
    root: gtk::Box,
    name: gtk::Label,
    bar: gtk::ProgressBar,
    value: gtk::Label,
}

impl ProcessRow {
    fn new() -> Self {
        let name = gtk::Label::new(None);
        name.add_css_class("caption");
        name.add_css_class("dim-label");
        name.set_xalign(0.0);
        name.set_size_request(84, -1);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let bar = gtk::ProgressBar::new();
        bar.add_css_class("edm-bar");
        bar.set_show_text(false);
        bar.set_hexpand(true);
        bar.set_valign(gtk::Align::Center);

        let value = gtk::Label::new(None);
        value.add_css_class("caption");
        value.add_css_class("edm-mono");
        value.set_xalign(1.0);
        value.set_size_request(108, -1);
        value.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.append(&name);
        root.append(&bar);
        root.append(&value);

        Self {
            root,
            name,
            bar,
            value,
        }
    }

    fn set(&self, entry: &ProcessEntry, by_cpu: bool, heaviest: u64) {
        self.root.set_visible(true);
        self.name.set_label(&entry.name);
        self.name
            .set_tooltip_text(Some(&format!("{} (PID {})", entry.name, entry.pid)));

        let (fraction, text) = if by_cpu {
            (
                entry.cpu.unwrap_or(0.0),
                match entry.cpu {
                    Some(cpu) => format!(
                        "{} · {}",
                        human_percent(cpu * 100.0),
                        human_bytes(entry.rss)
                    ),
                    None => human_bytes(entry.rss),
                },
            )
        } else {
            let fraction = if heaviest > 0 {
                entry.rss as f64 / heaviest as f64
            } else {
                0.0
            };
            (fraction, human_bytes(entry.rss))
        };

        self.bar.set_fraction(fraction.clamp(0.0, 1.0));
        self.value.set_label(&text);
        self.value.set_tooltip_text(Some(&text));
    }
}

pub fn processes(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::Processes { count, by_cpu } => ProcessOptions {
            count: *count,
            by_cpu: *by_cpu,
        },
        _ => unreachable!("processes called with the wrong view"),
    };

    let header = TileHeader::new(&definition.icon, "プロセス");
    let rows: Vec<ProcessRow> = (0..config.count.clamp(1, MAX_PROCESS_ROWS))
        .map(|_| ProcessRow::new())
        .collect();

    let caption = gtk::Label::new(None);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 5);
    root.set_vexpand(true);
    root.append(&header.root);
    for row in &rows {
        root.append(&row.root);
    }
    root.append(&caption);

    let previous: Rc<RefCell<Option<ProcessSample>>> = Rc::new(RefCell::new(None));
    tick_seconds(&root, PROCESSES_INTERVAL, move || {
        let entries = {
            let (entries, sample) = read_processes(previous.borrow().as_ref());
            *previous.borrow_mut() = Some(sample);
            entries
        };
        if entries.is_empty() {
            header.set_value("—");
            caption.set_label("プロセスを読めません");
            for row in &rows {
                row.root.set_visible(false);
            }
            return;
        }

        let total: u64 = entries.iter().map(|entry| entry.rss).sum();
        let ranked = rank_processes(entries, config.by_cpu);
        let heaviest = ranked.first().map_or(0, |entry| entry.rss);

        header.set_value(&ranked.len().to_string());
        caption.set_label(&format!(
            "上位 {} 件 · {}順 · 合計 {}",
            rows.len().min(ranked.len()),
            if config.by_cpu { "CPU" } else { "メモリ" },
            human_bytes(total)
        ));

        for (index, row) in rows.iter().enumerate() {
            match ranked.get(index) {
                Some(entry) => row.set(entry, config.by_cpu, heaviest),
                None => row.root.set_visible(false),
            }
        }
    });

    Ok(root.upcast())
}

// ---------------------------------------------------------------------------
// system
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SystemOptions {
    show_cpu: bool,
    show_memory: bool,
    show_load: bool,
    show_disk: bool,
    /// Filesystem shown by the disk row.
    disk_path: String,
}

/// One `label / bar / value` line.
struct SystemMetric {
    row: gtk::Box,
    bar: gtk::ProgressBar,
    value: gtk::Label,
}

impl SystemMetric {
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
    config: SystemOptions,
    cpu: SystemMetric,
    memory: SystemMetric,
    detail: gtk::Label,
    previous: RefCell<Option<CpuTimes>>,
}

impl SystemView {
    fn new(config: SystemOptions) -> Rc<Self> {
        Rc::new(Self {
            config,
            cpu: SystemMetric::new("CPU"),
            memory: SystemMetric::new("メモリ"),
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
}

pub fn system(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let _ = context;
    let config = match &definition.view {
        View::System {
            show_cpu,
            show_memory,
            show_load,
            show_disk,
            disk_path,
        } => SystemOptions {
            show_cpu: *show_cpu,
            show_memory: *show_memory,
            show_load: *show_load,
            show_disk: *show_disk,
            disk_path: disk_path.clone(),
        },
        _ => unreachable!("system called with the wrong view"),
    };

    let view = SystemView::new(config);
    let body = view.body();
    tick_seconds(&body, 2, move || view.refresh());

    Ok(body)
}
