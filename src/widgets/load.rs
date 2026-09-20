//! Load average widget: how much work is waiting, with a short history.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Bounds, Ink};
use crate::platform::procfs;
use crate::util::{History, human_percent};

use super::{Category, DetailPage, Readout, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "load";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "負荷",
    summary: "負荷平均と推移",
    icon: "speedometer-symbolic",
    category: Category::System,
    default_size: (340, 180),
    aspect: None,
    build,
    detail,
};

const HISTORY: usize = 90;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct LoadConfig {
    show_history: bool,
}

impl Default for LoadConfig {
    fn default() -> Self {
        Self { show_history: true }
    }
}

struct Plot {
    history: History,
    /// Load at which every core is busy, used as the top of the scale.
    scale: f64,
}

impl Plot {
    fn new() -> Self {
        Self {
            history: History::new(HISTORY),
            scale: procfs::cpu_count().max(1) as f64,
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: LoadConfig = context.config();
    let plot = Rc::new(RefCell::new(Plot::new()));
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
            draw(
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
    super::tick_seconds(&root, 2, move || {
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

fn draw(cr: &Context, width: f64, height: f64, plot: &Plot, ink: &Ink) {
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

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: LoadConfig = context.config();
    let page = DetailPage::new("負荷", "表示");

    match procfs::load_average() {
        Some([one, five, fifteen]) => {
            page.fact("1 分", &format!("{one:.2}"));
            page.fact("5 分", &format!("{five:.2}"));
            page.fact("15 分", &format!("{fifteen:.2}"));
            let cores = procfs::cpu_count();
            page.fact(
                "1 コアあたり (1 分)",
                &human_percent(one / cores.max(1) as f64 * 100.0),
            );
            page.fact("論理コア", &cores.to_string());
        }
        None => page.note("/proc/loadavg を読めませんでした。"),
    }

    page.switch(context, "show_history", "推移を表示", config.show_history);
    Ok(page.finish())
}
