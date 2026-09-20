//! CPU widget: per-core load as small columns, plus the total over time.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Bounds, Ink};
use crate::platform::procfs;
use crate::platform::procfs::CpuTimes;
use crate::util::{History, human_percent};

use super::{Category, DetailPage, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "cpu";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "CPU",
    summary: "全体とコアごとの使用率",
    icon: "power-profile-performance-symbolic",
    category: Category::System,
    default_size: (360, 240),
    aspect: None,
    build,
    detail,
};

/// Samples kept for the sparkline (one per second).
const HISTORY: usize = 90;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct CpuConfig {
    per_core: bool,
    show_history: bool,
}

impl Default for CpuConfig {
    fn default() -> Self {
        Self {
            per_core: true,
            show_history: true,
        }
    }
}

/// Everything the drawing closure needs. Holds no widgets, so it cannot keep a
/// removed tile alive.
struct Plot {
    history: History,
    cores: Vec<f64>,
}

impl Plot {
    fn new() -> Self {
        Self {
            history: History::new(HISTORY),
            cores: Vec::new(),
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: CpuConfig = context.config();
    let plot = Rc::new(RefCell::new(Plot::new()));
    let ink = Ink::new();

    let header = TileHeader::new(DESCRIPTOR.icon, "CPU");

    let area = gtk::DrawingArea::new();
    area.set_content_height(70);
    area.set_vexpand(true);
    area.set_hexpand(true);
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
    super::tick_seconds(&root, 1, move || {
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

fn draw(cr: &Context, width: f64, height: f64, plot: &Plot, ink: &Ink, config: CpuConfig) {
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

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: CpuConfig = context.config();
    let page = DetailPage::new("プロセッサ", "表示");

    if let Some(model) = procfs::cpu_model() {
        page.fact("モデル", &model);
    }
    page.fact("論理コア", &procfs::cpu_count().to_string());
    if let Some([one, five, fifteen]) = procfs::load_average() {
        page.fact("負荷平均", &format!("{one:.2} {five:.2} {fifteen:.2}"));
    }

    // Per-core usage needs two samples: show them as soon as the second one is
    // in, instead of blocking the dialog while sampling.
    let first = procfs::cpu_times_per_core();
    if first.len() > 1 {
        let rows: Vec<libadwaita::ActionRow> = (1..first.len())
            .map(|index| {
                let row = libadwaita::ActionRow::builder()
                    .title(format!("コア {index}"))
                    .subtitle("計測中…")
                    .build();
                page.fact_row(&row);
                row
            })
            .collect();

        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let second = procfs::cpu_times_per_core();
            for (row, (now, before)) in rows
                .iter()
                .zip(second.iter().skip(1).zip(first.iter().skip(1)))
            {
                let text = now
                    .usage_since(*before)
                    .map_or_else(|| "—".to_owned(), human_percent);
                row.set_subtitle(&text);
            }
        });
    }

    page.switch(context, "per_core", "コアごとに表示", config.per_core);
    page.switch(context, "show_history", "履歴を表示", config.show_history);
    Ok(page.finish())
}
