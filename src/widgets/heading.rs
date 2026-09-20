//! Heading widget: a text block used to organise a dashboard into sections.
//!
//! Not a monitor but a design element: a short accent bar, a title and an
//! optional subtitle.

use anyhow::Result;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Category, DetailPage, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "heading";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "見出し",
    summary: "セクション名などのテキスト",
    icon: "format-text-rich-symbolic",
    category: Category::Info,
    default_size: (400, 120),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct HeadingConfig {
    title: String,
    subtitle: String,
    accent: bool,
}

impl Default for HeadingConfig {
    fn default() -> Self {
        Self {
            title: "セクション".to_owned(),
            subtitle: String::new(),
            accent: true,
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: HeadingConfig = context.config();

    let title = gtk::Label::new(None);
    title.add_css_class("title-2");
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_label(&config.title);

    let subtitle = gtk::Label::new(Some(&config.subtitle));
    subtitle.add_css_class("dim-label");
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle.set_visible(!config.subtitle.is_empty());

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&subtitle);

    let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bar.add_css_class("edm-accent-bar");
    bar.set_size_request(5, -1);
    bar.set_valign(gtk::Align::Fill);
    bar.set_visible(config.accent);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.set_valign(gtk::Align::Center);
    root.set_vexpand(true);
    root.append(&bar);
    root.append(&text);
    Ok(root.upcast())
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: HeadingConfig = context.config();
    let page = DetailPage::new("内容", "表示");
    page.entry(context, "title", "見出し", &config.title);
    page.entry(context, "subtitle", "サブタイトル", &config.subtitle);
    page.switch(context, "accent", "アクセントバーを表示", config.accent);
    Ok(page.finish())
}
