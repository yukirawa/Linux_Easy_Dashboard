//! Plugin widget: a tile that is drawn from a user written `.toml` file.
//!
//! The definition lives in [`crate::plugin`]; this module turns one into a tile.
//! It samples the source on a worker thread (`plugin::sample` blocks on purpose)
//! and hands the result to the view the user asked for.
//!
//! A definition that was deleted or that cannot be parsed shows an error tile,
//! exactly like a broken built-in widget, so a plugin can never take anything
//! else down with it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyhow::{Result, anyhow};
use gtk::gio;
use gtk::prelude::*;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Bounds, Ink};
use crate::plugin::{self, Plugin, View};
use crate::util::History;

use super::{Category, DetailPage, Readout, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "plugin";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "プラグイン",
    summary: "自分で書いた定義 (.toml)",
    icon: "application-x-addon-symbolic",
    category: Category::Info,
    default_size: (300, 180),
    aspect: None,
    build,
    detail,
};

/// How many lines of raw output the detail view shows.
const PREVIEW_LINES: usize = 6;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PluginConfig {
    /// Which file in the plugin directory this tile shows.
    plugin: String,
    /// The catalogue revision this tile was built from. A definition that was
    /// saved or reloaded bumps it, which is how a tile notices that it has to be
    /// rebuilt — without this, editing a plugin would only show up on restart.
    revision: u64,
}

/// Writes a fresh sample into the tile. One of these is built per view, so the
/// tick itself stays free of branching.
type Apply = Rc<dyn Fn(Result<String, String>)>;

/// One interpretation of a sample, shared by every view.
struct Reading {
    /// The value as it should be shown, e.g. `3800 MHz` or the first line.
    text: String,
    /// The number behind it, when the output had one.
    number: Option<f64>,
    /// What went wrong, when the source failed.
    error: Option<String>,
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: PluginConfig = context.config();
    let definition = plugin::find(&config.plugin)
        .ok_or_else(|| {
            let name = if config.plugin.is_empty() {
                "(未設定)".to_owned()
            } else {
                config.plugin.clone()
            };
            anyhow!(
                "プラグイン「{name}」が見つかりません。メニューの「プラグインを再読み込み」か、\
                 plugins/ に .toml を置いてください"
            )
        })?
        .plugin;
    let definition = Rc::new(definition);

    let ink = Ink::new();
    let header = TileHeader::new(&definition.icon, &definition.name);

    let caption = gtk::Label::new(None);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    caption.set_wrap(true);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
    caption.set_label(&definition.summary);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);

    let apply = match &definition.view {
        View::Readout { label, .. } => readout_view(&root, &definition, label),
        View::Bar { .. } => bar_view(&root, &definition, &ink),
        View::Ring { .. } => ring_view(&root, &definition, &ink),
        View::Sparkline { history, .. } => sparkline_view(&root, &definition, &ink, *history),
        View::List { rows, .. } => list_view(&root, &definition, *rows),
        View::Text { wrap } => text_view(&root, &definition, *wrap),
    };

    root.append(&caption);
    let root = ink.wrap(&root);

    let revision = config.revision;
    let pending = Rc::new(Cell::new(false));
    let context_for_tick = context.clone();
    super::tick_seconds(&root, definition.refresh as u32, move || {
        // A definition that changed on disk means a different tile: patching the
        // revision asks the canvas to build this one again.
        let current = plugin::revision();
        if current != revision {
            context_for_tick.set("revision", serde_json::json!(current));
            return;
        }
        if pending.get() {
            return;
        }
        pending.set(true);

        let source = definition.source.clone();
        let hidden_when_idle = definition.hidden_when_idle;
        let apply = Rc::clone(&apply);
        let pending = Rc::clone(&pending);
        let context = context_for_tick.clone();
        glib::MainContext::default().spawn_local(async move {
            // `sample` runs a command or reads a file, so it never touches the
            // main thread.
            let outcome = match gio::spawn_blocking(move || plugin::sample(&source)).await {
                Ok(result) => result,
                Err(_) => Err("取得処理が異常終了しました".to_owned()),
            };
            if hidden_when_idle {
                context.set_hidden(outcome.as_ref().is_ok_and(|text| text.trim().is_empty()));
            }
            apply(outcome);
            pending.set(false);
        });
    });

    Ok(root.upcast())
}

// -- views ----------------------------------------------------------------

/// A big value with a caption, the base of every numeric view.
fn readout_view(root: &gtk::Box, definition: &Rc<Plugin>, label: &str) -> Apply {
    let readout = Readout::new();
    root.append(&readout.root);

    let definition = Rc::clone(definition);
    let label = label.to_owned();
    Rc::new(move |sample| {
        let reading = read(&definition, sample);
        readout.set(&reading.text, reading.error.as_deref().unwrap_or(&label));
    })
}

fn bar_view(root: &gtk::Box, definition: &Rc<Plugin>, ink: &Ink) -> Apply {
    let readout = Readout::new();
    let area = gtk::DrawingArea::new();
    area.set_content_height(10);
    area.set_hexpand(true);
    let fraction = Rc::new(Cell::new(0.0));
    let color = Rc::new(Cell::new(None::<gtk::gdk::RGBA>));
    {
        let fraction = Rc::clone(&fraction);
        let color = Rc::clone(&color);
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            graphics::prepare(cr);
            let width = f64::from(width);
            let height = f64::from(height);
            if width <= 4.0 || height <= 4.0 {
                return;
            }
            let thickness = (height * 0.7).clamp(4.0, 10.0);
            graphics::bar(
                cr,
                Bounds::new(0.0, (height - thickness) / 2.0, width, thickness),
                fraction.get(),
                &ink.alpha(0.12),
                &color.get().unwrap_or_else(|| ink.accent()),
            );
        });
    }

    root.append(&readout.root);
    root.append(&area);

    let definition = Rc::clone(definition);
    let label = view_label(definition.as_ref());
    let ink = ink.clone();
    Rc::new(move |sample| {
        let reading = read(&definition, sample);
        readout.set(&reading.text, reading.error.as_deref().unwrap_or(&label));
        match reading.number {
            Some(value) => {
                fraction.set(plugin::fraction(&definition, value));
                color.set(Some(value_color(&definition, value, &ink)));
            }
            None => fraction.set(0.0),
        }
        area.queue_draw();
    })
}

fn ring_view(root: &gtk::Box, definition: &Rc<Plugin>, ink: &Ink) -> Apply {
    let area = gtk::DrawingArea::new();
    area.set_content_width(96);
    area.set_content_height(96);
    area.set_vexpand(true);
    area.set_hexpand(true);
    let fraction = Rc::new(Cell::new(0.0));
    let color = Rc::new(Cell::new(None::<gtk::gdk::RGBA>));
    {
        let fraction = Rc::clone(&fraction);
        let color = Rc::clone(&color);
        let ink = ink.clone();
        area.set_draw_func(move |_, cr, width, height| {
            graphics::prepare(cr);
            let size = f64::from(width).min(f64::from(height));
            if size < 24.0 {
                return;
            }
            let thickness = (size * 0.14).max(6.0);
            graphics::ring(
                cr,
                (f64::from(width) / 2.0, f64::from(height) / 2.0),
                size / 2.0 - thickness / 2.0 - 1.0,
                thickness,
                fraction.get(),
                &ink.alpha(0.12),
                &color.get().unwrap_or_else(|| ink.accent()),
            );
        });
    }

    let readout = Readout::new();
    let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side.set_valign(gtk::Align::Center);
    side.set_hexpand(true);
    side.append(&readout.root);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_vexpand(true);
    row.append(&area);
    row.append(&side);
    root.append(&row);

    let definition = Rc::clone(definition);
    let label = view_label(definition.as_ref());
    let ink = ink.clone();
    Rc::new(move |sample| {
        let reading = read(&definition, sample);
        readout.set(&reading.text, reading.error.as_deref().unwrap_or(&label));
        match reading.number {
            Some(value) => {
                fraction.set(plugin::fraction(&definition, value));
                color.set(Some(value_color(&definition, value, &ink)));
            }
            None => fraction.set(0.0),
        }
        area.queue_draw();
    })
}

fn sparkline_view(root: &gtk::Box, definition: &Rc<Plugin>, ink: &Ink, history: usize) -> Apply {
    let readout = Readout::new();
    let area = gtk::DrawingArea::new();
    area.set_content_height(40);
    area.set_hexpand(true);
    area.set_vexpand(true);
    let samples: Rc<RefCell<History>> = Rc::new(RefCell::new(History::new(history)));
    {
        let samples = Rc::clone(&samples);
        let ink = ink.clone();
        let definition = Rc::clone(definition);
        area.set_draw_func(move |_, cr, width, height| {
            graphics::prepare(cr);
            if width <= 4 || height <= 4 {
                return;
            }
            let samples = samples.borrow();
            let scale = definition.view.number().and_then(|number| number.max);
            graphics::sparkline(
                cr,
                Bounds::new(0.0, 0.0, f64::from(width), f64::from(height)),
                &samples.samples(),
                scale,
                &ink.accent(),
            );
        });
    }

    root.append(&readout.root);
    root.append(&area);

    let definition = Rc::clone(definition);
    let label = view_label(definition.as_ref());
    Rc::new(move |sample| {
        let reading = read(&definition, sample);
        readout.set(&reading.text, reading.error.as_deref().unwrap_or(&label));
        if let Some(value) = reading.number {
            let mut samples = samples.borrow_mut();
            samples.push(value);
            samples.seed(2);
            area.queue_draw();
        }
    })
}

fn list_view(root: &gtk::Box, definition: &Rc<Plugin>, rows: usize) -> Apply {
    let mut lines = Vec::with_capacity(rows);
    for _ in 0..rows {
        let name = gtk::Label::new(None);
        name.add_css_class("caption");
        name.add_css_class("dim-label");
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let value = gtk::Label::new(None);
        value.add_css_class("caption");
        value.add_css_class("edm-mono");
        value.set_xalign(1.0);

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.append(&name);
        line.append(&value);
        root.append(&line);
        lines.push((line, name, value));
    }

    let definition = Rc::clone(definition);
    Rc::new(move |sample| {
        let text = match sample {
            Ok(text) => text,
            Err(message) => {
                if let Some((_, name, _)) = lines.first() {
                    name.set_label(&message);
                }
                for (line, _, _) in lines.iter().skip(1) {
                    line.set_visible(false);
                }
                return;
            }
        };
        let parsed = plugin::rows_of(&definition, &text);
        for (index, (line, name, value)) in lines.iter().enumerate() {
            match parsed.get(index) {
                Some((label, shown)) => {
                    line.set_visible(true);
                    name.set_label(label);
                    value.set_label(shown);
                }
                None => line.set_visible(index == 0 && parsed.is_empty()),
            }
        }
        if parsed.is_empty() {
            if let Some((_, name, _)) = lines.first() {
                name.set_label("—");
            }
        }
    })
}

fn text_view(root: &gtk::Box, definition: &Rc<Plugin>, wrap: bool) -> Apply {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_yalign(0.0);
    label.set_vexpand(true);
    label.set_wrap(wrap);
    if wrap {
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    }
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_lines(PREVIEW_LINES as i32);
    root.append(&label);

    let definition = Rc::clone(definition);
    Rc::new(move |sample| match sample {
        Ok(text) if text.trim().is_empty() => label.set_label(definition.view.unit()),
        Ok(text) => label.set_label(&text),
        Err(message) => label.set_label(&message),
    })
}

// -- helpers --------------------------------------------------------------

/// Turns a sample into a value, a number and an error, the way every view wants
/// it.
fn read(definition: &Plugin, sample: Result<String, String>) -> Reading {
    match sample {
        Err(message) => Reading {
            text: "—".to_owned(),
            number: None,
            error: Some(message),
        },
        Ok(text) if text.trim().is_empty() => Reading {
            text: "—".to_owned(),
            number: None,
            error: None,
        },
        Ok(text) => match plugin::number_of(definition, &text) {
            Some(value) => Reading {
                text: plugin::format_value(definition, value),
                number: Some(value),
                error: None,
            },
            None => Reading {
                text: first_line(&text),
                number: None,
                error: None,
            },
        },
    }
}

/// Meters are accent coloured, unless the definition names a threshold.
///
/// Deliberately *not* the generic 75 %/90 % ramp: a plugin's own `warn_at` is
/// the only threshold the user asked for.
fn value_color(definition: &Plugin, value: f64, ink: &Ink) -> gtk::gdk::RGBA {
    match definition.view.number().and_then(|number| number.warn_at) {
        Some(threshold) if value >= threshold => ink.warn(),
        _ => ink.accent(),
    }
}

/// The caption of a numeric view: the definition's `label`, or its name.
fn view_label(definition: &Plugin) -> String {
    match &definition.view {
        View::Readout { label, .. } if !label.trim().is_empty() => label.trim().to_owned(),
        _ => definition.name.clone(),
    }
}

/// The first line that is not blank.
fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map_or_else(|| "—".to_owned(), |line| line.trim().to_owned())
}

// -- detail ---------------------------------------------------------------

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: PluginConfig = context.config();
    let page = DetailPage::new("プラグイン", "定義");

    let Some(loaded) = plugin::find(&config.plugin) else {
        page.note(&format!(
            "プラグイン「{}」が見つかりません。plugins/ に .toml を置くか、\
             メニューの「プラグインを再読み込み」を実行してください。",
            config.plugin
        ));
        return Ok(page.finish());
    };
    let definition = loaded.plugin.clone();

    page.fact("ID", &definition.id);
    page.fact("名前", &definition.name);
    page.fact("カテゴリ", definition.category.label());
    page.fact("更新間隔", &format!("{} 秒", definition.refresh));
    page.fact("取得元", &source_label(&definition));
    page.fact("定義ファイル", &loaded.path.display().to_string());
    if definition.hidden_when_idle {
        page.fact("空のとき", "タイルを隠す");
    }

    // The current sample: fetched in the background, because sampling runs a
    // command and the dialog must not wait for it.
    let preview = gtk::Label::new(Some("取得中…"));
    preview.add_css_class("caption");
    preview.add_css_class("dim-label");
    preview.add_css_class("edm-code");
    preview.set_xalign(0.0);
    preview.set_wrap(true);
    preview.set_selectable(true);
    page.custom(&preview);
    {
        let source = definition.source.clone();
        glib::MainContext::default().spawn_local(async move {
            let outcome = match gio::spawn_blocking(move || plugin::sample(&source)).await {
                Ok(result) => result,
                Err(_) => Err("取得処理が異常終了しました".to_owned()),
            };
            preview.set_label(&match outcome {
                Ok(text) if text.is_empty() => "（空）".to_owned(),
                Ok(text) => text
                    .lines()
                    .take(PREVIEW_LINES)
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(message) => format!("⚠ {message}"),
            });
        });
    }

    page.custom(&editor(context, &definition));
    Ok(page.finish())
}

/// What the definition reads, in one line.
fn source_label(definition: &Plugin) -> String {
    match &definition.source {
        plugin::Source::Command { run, timeout_secs } => {
            format!("コマンド ({timeout_secs} 秒): {run}")
        }
        plugin::Source::File { path } => format!("ファイル: {path}"),
    }
}

/// The in-app definition editor: a plugin can be written and fixed without
/// leaving the dashboard.
fn editor(context: &WidgetContext, definition: &Plugin) -> gtk::Widget {
    let title = gtk::Label::new(Some("定義 (.toml) — 保存するとすぐ反映されます"));
    title.add_css_class("caption");
    title.add_css_class("dim-label");
    title.set_xalign(0.0);

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&plugin::read_source(&definition.id).unwrap_or_default());

    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .accepts_tab(false)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(10)
        .right_margin(10)
        .build();

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(280)
        .child(&view)
        .build();
    scrolled.add_css_class("card");

    let status = gtk::Label::new(None);
    status.add_css_class("caption");
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_wrap(true);

    let save = gtk::Button::with_label("保存");
    save.add_css_class("suggested-action");
    let delete = gtk::Button::with_label("削除");
    delete.add_css_class("destructive-action");

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.append(&save);
    buttons.append(&delete);

    let group = gtk::Box::new(gtk::Orientation::Vertical, 8);
    group.append(&title);
    group.append(&scrolled);
    group.append(&buttons);
    group.append(&status);

    save.connect_clicked({
        let context = context.clone();
        let id = definition.id.clone();
        let buffer = buffer.clone();
        let status = status.clone();
        move |_| {
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            let message = match plugin::write_source(&id, &text) {
                Ok(()) => "保存しました。タイルを更新します。".to_owned(),
                // Written anyway, so the user can carry on fixing it.
                Err(e) => format!("保存しましたが読み込めません: {e}"),
            };
            status.set_label(&message);
            refresh_tiles(&context);
        }
    });

    delete.connect_clicked({
        let context = context.clone();
        let id = definition.id.clone();
        let status = status.clone();
        let button = delete.clone();
        move |_| {
            let dialog = libadwaita::AlertDialog::builder()
                .heading("このプラグインを削除しますか？")
                .body(format!(
                    "{id}.toml を削除します。タイルは残るので、個別に消してください。"
                ))
                .build();
            dialog.add_response("cancel", "キャンセル");
            dialog.add_response("delete", "削除");
            dialog.set_response_appearance("delete", libadwaita::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            dialog.connect_response(None, {
                let context = context.clone();
                let id = id.clone();
                let status = status.clone();
                move |_, response| {
                    if response != "delete" {
                        return;
                    }
                    let message = match plugin::remove(&id) {
                        Ok(()) => "削除しました。".to_owned(),
                        Err(e) => e,
                    };
                    status.set_label(&message);
                    refresh_tiles(&context);
                }
            });
            dialog.present(button.root().and_downcast::<gtk::Window>().as_ref());
        }
    });

    group.upcast()
}

/// Asks every plugin tile to rebuild itself.
///
/// A definition change is not a *setting* of one instance, so it cannot be a
/// patch of its own: bumping the revision makes each plugin instance notice on
/// its next tick, and the one whose editor is open straight away.
fn refresh_tiles(context: &WidgetContext) {
    context.set("revision", serde_json::json!(plugin::revision()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(view: &str) -> Plugin {
        plugin::parse(&format!(
            "id = \"t\"\nname = \"テスト\"\n\n[source]\nkind = \"command\"\nrun = \"printf 1\"\n\n[view]\n{view}\n"
        ))
        .expect("the test definition must parse")
    }

    #[test]
    fn numbers_become_the_value_line() {
        let plugin = definition("kind = \"ring\"\nmin = 0\nmax = 100\nunit = \"%\"");
        let reading = read(&plugin, Ok("42\n".to_owned()));
        assert_eq!(reading.text, "42 %");
        assert_eq!(reading.number, Some(42.0));
        assert!(reading.error.is_none());
    }

    #[test]
    fn text_output_is_shown_when_it_is_not_a_number() {
        let plugin = definition("kind = \"readout\"");
        let reading = read(&plugin, Ok("hello\nworld\n".to_owned()));
        assert_eq!(reading.text, "hello");
        assert_eq!(reading.number, None);
    }

    #[test]
    fn empty_output_and_failures_are_distinguishable() {
        let plugin = definition("kind = \"readout\"");
        let empty = read(&plugin, Ok(String::new()));
        assert_eq!(empty.text, "—");
        assert!(empty.error.is_none());

        let failed = read(&plugin, Err("読めません".to_owned()));
        assert_eq!(failed.text, "—");
        assert_eq!(failed.error.as_deref(), Some("読めません"));
    }

    #[test]
    fn a_threshold_colours_the_meter() {
        let ink = Ink::new();
        let plain = definition("kind = \"bar\"\nmin = 0\nmax = 100");
        assert_eq!(value_color(&plain, 99.0, &ink), ink.accent());

        let warned = definition("kind = \"bar\"\nmin = 0\nmax = 100\nwarn_at = 80");
        assert_eq!(value_color(&warned, 80.0, &ink), ink.warn());
        assert_eq!(value_color(&warned, 79.0, &ink), ink.accent());
    }

    #[test]
    fn the_caption_prefers_the_definition_label() {
        let labelled = definition("kind = \"readout\"\nlabel = \"室温\"");
        assert_eq!(view_label(&labelled), "室温");
        let unlabelled = definition("kind = \"readout\"");
        assert_eq!(view_label(&unlabelled), "テスト");
    }

    #[test]
    fn the_first_line_is_used_for_text_views() {
        assert_eq!(first_line("\n\n  こんにちは  \n次\n"), "こんにちは");
        assert_eq!(first_line("   "), "—");
    }
}
