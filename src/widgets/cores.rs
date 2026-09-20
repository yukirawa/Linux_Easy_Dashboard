//! Core heatmap widget: one cell per logical core, shaded by how busy it is.
//!
//! The grid adapts to the tile's shape, so it reads well as a wide strip or as a
//! square block. Load is a difference between two samples, so the very first
//! tick only establishes a baseline.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Ink};
use crate::platform::procfs;
use crate::platform::procfs::CpuTimes;
use crate::util::human_percent;

use super::{Category, DetailPage, Spin, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "cores";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "コア",
    summary: "コアごとの使用率をヒートマップで",
    icon: "view-grid-symbolic",
    category: Category::System,
    default_size: (340, 240),
    aspect: None,
    build,
    detail,
};

/// How long between samples. Load is measured over this window.
const INTERVAL: u32 = 1;

/// Cells beyond this are not drawn: they would be a pixel each. The caption
/// says how many are missing instead of pretending the machine is smaller.
const MAX_CELLS: usize = 128;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct CoresConfig {
    /// Fixed column count, or 0 to fit the tile's shape.
    columns: usize,
    /// Colour by load (accent, then warning, then error).
    warn_by_usage: bool,
}

impl Default for CoresConfig {
    fn default() -> Self {
        Self {
            columns: 0,
            warn_by_usage: true,
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: CoresConfig = context.config();
    let usage: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let ink = Ink::new();

    let header = TileHeader::new(DESCRIPTOR.icon, "コア");

    let area = gtk::DrawingArea::new();
    area.set_content_height(80);
    area.set_vexpand(true);
    area.set_hexpand(true);
    {
        let usage = usage.clone();
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            draw(
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
    super::tick_seconds(&root, INTERVAL, move || {
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

        *usage_for_tick.borrow_mut() = fractions(cores);
        area.queue_draw();
    });

    Ok(root.upcast())
}

fn draw(cr: &Context, width: f64, height: f64, usage: &[f64], ink: &Ink, config: CoresConfig) {
    graphics::prepare(cr);
    if usage.is_empty() || width < 12.0 || height < 12.0 {
        return;
    }

    let count = usage.len();
    let columns = columns(count, config.columns, width / height);
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
fn fractions(percents: &[f64]) -> Vec<f64> {
    percents
        .iter()
        .take(MAX_CELLS)
        .map(|percent| (percent / 100.0).clamp(0.0, 1.0))
        .collect()
}

/// Number of columns for `count` cells in a `aspect` (width/height) box.
fn columns(count: usize, configured: usize, aspect: f64) -> usize {
    let count = count.max(1);
    if configured > 0 {
        return configured.min(count);
    }
    // Cells as square as the box allows.
    ((count as f64 * aspect.clamp(0.25, 4.0)).sqrt().ceil() as usize).clamp(1, count)
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: CoresConfig = context.config();
    let page = DetailPage::new("プロセッサ", "表示");

    if let Some(model) = procfs::cpu_model() {
        page.fact("モデル", &model);
    }
    page.fact("論理コア", &procfs::cpu_count().to_string());
    if let Some([one, five, fifteen]) = procfs::load_average() {
        page.fact("負荷平均", &format!("{one:.2} {five:.2} {fifteen:.2}"));
    }

    // Per-core load needs two samples; fill the rows in as soon as the second
    // one is in rather than blocking the dialog.
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

        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            let second = procfs::cpu_times_per_core();
            for (row, (now, before)) in rows
                .iter()
                .zip(second.iter().skip(1).zip(first.iter().skip(1)))
            {
                row.set_subtitle(
                    &now.usage_since(*before)
                        .map_or_else(|| "—".to_owned(), human_percent),
                );
            }
        });
    }

    page.spin(
        context,
        Spin::new(
            "columns",
            "列数 (0 で自動)",
            config.columns as f64,
            0.0,
            8.0,
        ),
    );
    page.switch(
        context,
        "warn_by_usage",
        "使用率で色を変える",
        config.warn_by_usage,
    );
    Ok(page.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_columns_follow_the_tile_shape() {
        assert_eq!(columns(16, 0, 1.0), 4);
        assert_eq!(columns(16, 0, 4.0), 8);
        assert_eq!(columns(8, 0, 0.25), 2);
        // A single core always gets a single cell.
        assert_eq!(columns(1, 0, 3.0), 1);
    }

    #[test]
    fn a_configured_column_count_is_respected_within_bounds() {
        assert_eq!(columns(8, 3, 1.0), 3);
        // Never more columns than cells.
        assert_eq!(columns(2, 8, 1.0), 2);
        // A silly ratio cannot produce zero or absurd columns.
        assert_eq!(columns(16, 0, 0.0), 2);
        assert_eq!(columns(16, 0, 1000.0), 8);
    }

    #[test]
    fn percentages_become_shading_without_saturating() {
        assert_eq!(fractions(&[0.0, 50.0, 100.0]), [0.0, 0.5, 1.0]);
        // A quiet core must not look busy: this is the threshold that decides
        // between the accent and the warning colour.
        assert!(fractions(&[3.0])[0] < 0.75);
        // Anything beyond the first 128 cores is not drawn.
        assert_eq!(fractions(&vec![1.0; MAX_CELLS + 10]).len(), MAX_CELLS);
    }
}
