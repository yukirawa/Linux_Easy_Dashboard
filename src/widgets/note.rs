//! Note widget: a short piece of text pinned to the dashboard.
//!
//! The tile is read-only so that clicking it opens the editor; editing happens
//! in the detail view and is saved with a short debounce.

use std::time::Duration;

use anyhow::Result;
use gtk::prelude::*;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Category, DetailPage, WidgetContext, WidgetDescriptor};
use crate::ui::debounce::Debounce;

pub const KIND: &str = "note";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "メモ",
    summary: "短いテキストを貼っておく",
    icon: "view-paged-symbolic",
    category: Category::Info,
    default_size: (340, 220),
    aspect: None,
    build,
    detail,
};

/// Delay between the last keystroke and saving.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct NoteConfig {
    title: String,
    text: String,
}

impl Default for NoteConfig {
    fn default() -> Self {
        Self {
            title: "メモ".to_owned(),
            text: String::new(),
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: NoteConfig = context.config();

    let title = gtk::Label::new(Some(&config.title));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_visible(!config.title.is_empty());

    let placeholder = if config.text.trim().is_empty() {
        "クリックして入力"
    } else {
        &config.text
    };
    let text = gtk::Label::new(Some(placeholder));
    text.set_xalign(0.0);
    text.set_yalign(0.0);
    text.set_wrap(true);
    text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.set_lines(8);
    text.set_vexpand(true);
    text.set_selectable(false);
    if config.text.trim().is_empty() {
        text.add_css_class("dim-label");
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&title);
    root.append(&text);
    Ok(root.upcast())
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: NoteConfig = context.config();

    let title_row = super::text_row(
        context,
        "タイトル",
        &config.title,
        |text| serde_json::json!({ "title": text }),
    );

    let group = libadwaita::PreferencesGroup::new();
    group.set_title("内容");
    group.add(&title_row);

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&config.text);

    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .accepts_tab(false)
        .build();

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(220)
        .child(&view)
        .build();
    scrolled.add_css_class("card");

    // Save shortly after typing stops, so the layout file is not rewritten on
    // every keystroke. The debounce forgets its pending source before it fires,
    // which is what makes re-arming from the next keystroke safe.
    let debounce = Debounce::new(SAVE_DEBOUNCE);
    let context_for_text = context.clone();
    buffer.connect_changed(move |buffer| {
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        let context = context_for_text.clone();
        debounce.schedule(move || {
            context.set("text", serde_json::Value::String(text));
        });
    });

    let page = DetailPage::new("メモ", "設定");
    page.custom(&group);
    page.custom(&scrolled);
    page.note("入力は自動で保存されます。");
    Ok(page.finish())
}
