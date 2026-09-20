//! Network widget: throughput of one interface, with a short history per
//! direction.
//!
//! Counters come from `/proc/net/dev`; the widget keeps the previous sample to
//! turn cumulative bytes into rates.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Bounds, Ink};
use crate::platform::procfs;
use crate::platform::procfs::NetCounters;
use crate::util::{History, human_rate};

use super::{Category, DetailPage, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "network";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "ネットワーク",
    summary: "送受信の速度と推移",
    icon: "network-wired-symbolic",
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
struct NetworkConfig {
    /// Interface to watch. Empty means "follow the default route".
    interface: String,
    show_history: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            interface: String::new(),
            show_history: true,
        }
    }
}

/// Drawing input: both directions, scaled together so the curves are
/// comparable.
struct Plot {
    down: History,
    up: History,
    peak: f64,
}

impl Plot {
    fn new() -> Self {
        Self {
            down: History::new(HISTORY),
            up: History::new(HISTORY),
            peak: 1.0,
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: NetworkConfig = context.config();
    let plot = Rc::new(RefCell::new(Plot::new()));
    let ink = Ink::new();

    let header = TileHeader::new(DESCRIPTOR.icon, "ネットワーク");
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
            draw(
                cr,
                f64::from(width),
                f64::from(height),
                &plot.borrow(),
                &ink,
            );
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
    super::tick_seconds(&root, INTERVAL, move || {
        let Some((name, counters)) = current(&config) else {
            header.set_value("—");
            down.set_label("インターフェースが見つかりません");
            up.set_label("");
            return;
        };

        let rates = {
            let mut previous = previous.borrow_mut();
            let rates = previous.map(|before| counters.rates_since(before, f64::from(INTERVAL)));
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
fn current(config: &NetworkConfig) -> Option<(String, NetCounters)> {
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

fn draw(cr: &Context, width: f64, height: f64, plot: &Plot, ink: &Ink) {
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

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: NetworkConfig = context.config();
    let page = DetailPage::new("インターフェース", "表示");

    for (name, counters) in procfs::network_counters() {
        let default = procfs::default_interface().is_some_and(|default| default == name);
        let title = if default {
            format!("{name} (既定)")
        } else {
            name.clone()
        };
        page.fact(
            &title,
            &format!(
                "受信 {} · 送信 {}",
                crate::util::human_bytes(counters.received),
                crate::util::human_bytes(counters.transmitted)
            ),
        );
    }

    page.entry(
        context,
        "interface",
        "監視するインターフェース (空で既定ルート)",
        &config.interface,
    );
    page.switch(context, "show_history", "推移を表示", config.show_history);
    Ok(page.finish())
}
