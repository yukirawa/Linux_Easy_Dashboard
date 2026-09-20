//! Memory widget: a ring gauge for RAM, plus swap in the detail view.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Ink};
use crate::platform::procfs;
use crate::util::{human_bytes, human_percent};

use super::{Category, DetailPage, Readout, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "memory";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "メモリ",
    summary: "使用量のリングゲージとスワップ",
    icon: "media-flash-symbolic",
    category: Category::System,
    default_size: (320, 220),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct MemoryConfig {
    /// Show the swap line under the readout.
    show_swap: bool,
    /// Colour the ring by usage (accent, then warning, then error).
    warn_by_usage: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            show_swap: true,
            warn_by_usage: true,
        }
    }
}

/// What the drawing closure needs, without holding on to any widget.
#[derive(Default)]
struct Gauge {
    fraction: f64,
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: MemoryConfig = context.config();

    let gauge = Rc::new(RefCell::new(Gauge::default()));
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
            draw(
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
    super::tick_seconds(&root, 2, move || {
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

fn draw(cr: &Context, width: f64, height: f64, gauge: &Gauge, ink: &Ink, config: MemoryConfig) {
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

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: MemoryConfig = context.config();
    let page = DetailPage::new("使用状況", "表示");

    match procfs::memory() {
        Some(memory) => {
            page.fact(
                "使用中",
                &format!(
                    "{} / {} ({})",
                    human_bytes(memory.used()),
                    human_bytes(memory.total),
                    human_percent(memory.used() as f64 / memory.total as f64 * 100.0)
                ),
            );
            page.fact("利用可能", &human_bytes(memory.available));
            if memory.swap_total > 0 {
                page.fact(
                    "スワップ",
                    &format!(
                        "{} / {}",
                        human_bytes(memory.swap_used()),
                        human_bytes(memory.swap_total)
                    ),
                );
            } else {
                page.fact("スワップ", "未設定");
            }
        }
        None => page.note("/proc/meminfo を読めませんでした。"),
    }

    page.switch(context, "show_swap", "スワップを表示", config.show_swap);
    page.switch(
        context,
        "warn_by_usage",
        "使用率で色を変える",
        config.warn_by_usage,
    );
    Ok(page.finish())
}
