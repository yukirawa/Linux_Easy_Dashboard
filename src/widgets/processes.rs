//! Process widget: the heaviest processes as a compact ranked list.
//!
//! Ranking by CPU needs two samples (a process's CPU time only means something
//! as a difference), so the first tick falls back to memory and the CPU share
//! appears from the second one on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use gtk::prelude::*;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use crate::platform::procfs;
use crate::util::{human_bytes, human_percent};

use super::{Category, DetailPage, Spin, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "processes";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "プロセス",
    summary: "重いプロセスの順位",
    icon: "view-list-symbolic",
    category: Category::System,
    default_size: (380, 240),
    aspect: None,
    build,
    detail,
};

/// How often the list is rebuilt. Reading every `/proc/<pid>` is heavier than
/// the other widgets, so this one is slower than the rest.
const INTERVAL: u32 = 3;

/// Rows shown in the detail view.
const DETAIL_ROWS: usize = 12;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct ProcessConfig {
    /// Rows in the tile.
    count: usize,
    /// Rank by CPU share instead of memory.
    by_cpu: bool,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            count: 5,
            by_cpu: false,
        }
    }
}

/// A process together with the load measured since the previous sample.
#[derive(Debug, Clone)]
struct Entry {
    pid: i32,
    name: String,
    rss: u64,
    /// Share of the whole machine's CPU since the last sample, when known.
    cpu: Option<f64>,
}

/// The cumulative counters of one sample: total CPU ticks, then per process.
type Sample = (u64, HashMap<i32, u64>);

/// Reads every process and pairs it with the load since `previous`.
fn read_processes(previous: Option<&Sample>) -> (Vec<Entry>, Sample) {
    let processes = procfs::processes();
    let total = procfs::cpu_times().map_or(0, |times| times.total);
    let elapsed = previous.map(|(before, _)| total.saturating_sub(*before));

    let entries = processes
        .iter()
        .map(|process| Entry {
            pid: process.pid,
            name: process.name.clone(),
            rss: process.rss,
            cpu: share(process, previous, elapsed),
        })
        .collect();
    let ticks = processes
        .iter()
        .map(|process| (process.pid, process.cpu_ticks))
        .collect();
    (entries, (total, ticks))
}

/// The part of the machine's CPU time one process used since the last sample.
fn share(
    process: &procfs::Process,
    previous: Option<&Sample>,
    elapsed_ticks: Option<u64>,
) -> Option<f64> {
    let elapsed = elapsed_ticks.filter(|ticks| *ticks > 0)?;
    let before = previous?.1.get(&process.pid)?;
    let ticks = process.cpu_ticks.saturating_sub(*before);
    Some((ticks as f64 / elapsed as f64).clamp(0.0, 1.0))
}

/// Heaviest first. Processes without a CPU share sort as if they used nothing,
/// which is what makes the first sample fall back to memory.
fn rank(mut entries: Vec<Entry>, by_cpu: bool) -> Vec<Entry> {
    entries.sort_by(|a, b| {
        let key = |entry: &Entry| {
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
struct Row {
    root: gtk::Box,
    name: gtk::Label,
    bar: gtk::ProgressBar,
    value: gtk::Label,
}

impl Row {
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

    fn set(&self, entry: &Entry, by_cpu: bool, heaviest: u64) {
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

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: ProcessConfig = context.config();

    let header = TileHeader::new(DESCRIPTOR.icon, "プロセス");
    let rows: Vec<Row> = (0..config.count.clamp(1, DETAIL_ROWS))
        .map(|_| Row::new())
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

    let previous: Rc<RefCell<Option<Sample>>> = Rc::new(RefCell::new(None));
    super::tick_seconds(&root, INTERVAL, move || {
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
        let ranked = rank(entries, config.by_cpu);
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

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: ProcessConfig = context.config();
    let page = DetailPage::new("上位プロセス", "表示");

    let (entries, sample) = read_processes(None);
    if entries.is_empty() {
        page.note("/proc からプロセスを読めませんでした。");
    }
    let top: Vec<Entry> = rank(entries, config.by_cpu)
        .into_iter()
        .take(DETAIL_ROWS)
        .collect();

    let rows: Vec<libadwaita::ActionRow> = top
        .iter()
        .map(|entry| {
            let row = libadwaita::ActionRow::builder()
                .title(&entry.name)
                .subtitle(format!("PID {} · {}", entry.pid, human_bytes(entry.rss)))
                .build();
            page.fact_row(&row);
            row
        })
        .collect();

    if !rows.is_empty() {
        // The CPU share is a difference, so wait for a second sample instead of
        // blocking the dialog on the first one.
        let entries = top.clone();
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let (later, _) = read_processes(Some(&sample));
            let shares: HashMap<i32, Option<f64>> =
                later.iter().map(|entry| (entry.pid, entry.cpu)).collect();
            for (row, entry) in rows.iter().zip(entries.iter()) {
                let cpu = shares
                    .get(&entry.pid)
                    .copied()
                    .flatten()
                    .map_or_else(|| "—".to_owned(), |cpu| human_percent(cpu * 100.0));
                row.set_subtitle(&format!(
                    "PID {} · {} · CPU {cpu}",
                    entry.pid,
                    human_bytes(entry.rss)
                ));
            }
        });
    }

    page.spin(
        context,
        Spin::new(
            "count",
            "表示する件数",
            config.count as f64,
            1.0,
            DETAIL_ROWS as f64,
        ),
    );
    page.switch(context, "by_cpu", "CPU の使用率で並べる", config.by_cpu);
    page.note("CPU の値は詳細を開いてからの短い区間の平均です。");
    Ok(page.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, name: &str, rss: u64, cpu_ticks: u64) -> procfs::Process {
        procfs::Process {
            pid,
            name: name.to_owned(),
            rss,
            cpu_ticks,
        }
    }

    #[test]
    fn cpu_share_is_the_difference_over_the_elapsed_ticks() {
        let sample: Sample = (1_000, [(1, 100), (2, 100)].into_iter().collect());
        // One process used 50 of the 500 ticks that passed: a tenth of the box.
        let entry = share(&process(1, "busy", 0, 150), Some(&sample), Some(500));
        assert_eq!(entry, Some(0.1));

        // Without a previous sample there is nothing to difference.
        assert_eq!(share(&process(1, "busy", 0, 150), None, Some(500)), None);
        // A process that just appeared is not counted either.
        assert_eq!(
            share(&process(3, "new", 0, 10), Some(&sample), Some(500)),
            None
        );
        // No time passed (a very fast tick): the share is unknown, not infinite.
        assert_eq!(
            share(&process(1, "busy", 0, 150), Some(&sample), Some(0)),
            None
        );
    }

    #[test]
    fn ranking_switches_between_memory_and_cpu() {
        let entries = vec![
            Entry {
                pid: 1,
                name: "quiet".to_owned(),
                rss: 4_000,
                cpu: Some(0.01),
            },
            Entry {
                pid: 2,
                name: "busy".to_owned(),
                rss: 1_000,
                cpu: Some(0.4),
            },
        ];

        let by_memory = rank(entries.clone(), false);
        assert_eq!(by_memory[0].name, "quiet");
        let by_cpu = rank(entries, true);
        assert_eq!(by_cpu[0].name, "busy");
    }

    #[test]
    fn ranking_falls_back_to_memory_before_cpu_is_known() {
        let entries = vec![
            Entry {
                pid: 1,
                name: "small".to_owned(),
                rss: 1_000,
                cpu: None,
            },
            Entry {
                pid: 2,
                name: "big".to_owned(),
                rss: 9_000,
                cpu: None,
            },
        ];
        let ranked = rank(entries, true);
        assert_eq!(ranked[0].name, "big");
    }
}
