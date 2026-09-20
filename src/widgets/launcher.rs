//! Launcher widget: pinned applications, started through `GIO`.
//!
//! Applications are `.desktop` entries as the desktop itself sees them, so
//! launching goes through the same machinery as the system menu — no shell, no
//! hardcoded commands.

use std::rc::Rc;

use anyhow::Result;
use gtk::prelude::*;
use log::debug;
use serde::{Deserialize, Serialize};

use super::{Category, DetailPage, Spin, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "launcher";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "アプリランチャー",
    summary: "よく使うアプリを起動",
    icon: "view-app-grid-symbolic",
    category: Category::Apps,
    default_size: (360, 200),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct LauncherConfig {
    /// Desktop entry ids, e.g. `org.gnome.Nautilus.desktop`.
    apps: Vec<String>,
    columns: usize,
    show_labels: bool,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            apps: Vec::new(),
            columns: 4,
            show_labels: false,
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: LauncherConfig = context.config();

    let grid = gtk::Grid::builder()
        .column_spacing(6)
        .row_spacing(6)
        .column_homogeneous(true)
        .row_homogeneous(true)
        .build();
    grid.set_valign(gtk::Align::Center);
    grid.set_vexpand(true);

    let hint = gtk::Label::new(Some("詳細からアプリを選んでください"));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_justify(gtk::Justification::Center);
    hint.set_valign(gtk::Align::Center);
    hint.set_vexpand(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);

    let entries = resolve(&config.apps);
    if entries.is_empty() {
        root.append(&hint);
    } else {
        let columns = config.columns.clamp(1, 8) as i32;
        for (index, (id, app)) in entries.iter().enumerate() {
            let button = app_button(app, config.show_labels);
            button.set_tooltip_text(Some(id));
            grid.attach(
                &button,
                index as i32 % columns,
                index as i32 / columns,
                1,
                1,
            );
        }
        root.append(&grid);
    }

    Ok(root.upcast())
}

/// One launcher tile: the application icon, optionally with its name.
fn app_button(app: &gtk::gio::AppInfo, show_label: bool) -> gtk::Button {
    let image = gtk::Image::from_gicon(
        &app.icon()
            .unwrap_or_else(|| gtk::gio::ThemedIcon::new("application-x-executable").upcast()),
    );
    image.set_pixel_size(if show_label { 32 } else { 36 });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_valign(gtk::Align::Center);
    content.set_halign(gtk::Align::Center);
    content.append(&image);

    if show_label {
        let label = gtk::Label::new(Some(app.display_name().as_str()));
        label.add_css_class("caption");
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(12);
        content.append(&label);
    }

    let button = gtk::Button::builder()
        .has_frame(false)
        .child(&content)
        .tooltip_text(app.display_name().as_str())
        .build();
    button.add_css_class("edm-launcher-button");

    let app = app.clone();
    button.connect_clicked(move |_| launch(&app));
    button
}

/// Starts an application the way the desktop would.
fn launch(app: &gtk::gio::AppInfo) {
    let files: [gtk::gio::File; 0] = [];
    if let Err(e) = app.launch(&files, None::<&gtk::gio::AppLaunchContext>) {
        // Nothing to show in a toast from here; the log is the honest place.
        log::warn!("{} を起動できません: {e}", app.display_name());
        return;
    }
    debug!("起動しました: {}", app.display_name());
}

/// Looks up the pinned ids, dropping anything that is no longer installed.
fn resolve(ids: &[String]) -> Vec<(String, gtk::gio::AppInfo)> {
    let installed = gtk::gio::AppInfo::all();
    ids.iter()
        .filter_map(|id| {
            let app = installed
                .iter()
                .find(|app| app.id().as_deref() == Some(id.as_str()))?;
            Some((id.clone(), app.clone()))
        })
        .collect()
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: LauncherConfig = context.config();
    let page = DetailPage::new("起動するアプリ", "表示");

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("アプリを検索"));
    search.set_margin_top(6);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    let mut apps: Vec<gtk::gio::AppInfo> = gtk::gio::AppInfo::all()
        .into_iter()
        .filter(|app| app.should_show())
        .collect();
    apps.sort_by_key(|app| app.display_name().to_lowercase());

    let mut toggles: Vec<(gtk::gio::AppInfo, libadwaita::SwitchRow)> = Vec::new();
    let pinned = Rc::new(std::cell::RefCell::new(config.apps.clone()));
    for app in apps {
        let id = app.id().map_or_else(String::new, |id| id.to_string());
        let row = libadwaita::SwitchRow::builder()
            .title(app.display_name().as_str())
            .subtitle(&id)
            .active(config.apps.contains(&id))
            .build();

        let context = context.clone();
        let pinned = pinned.clone();
        let app_id = id.clone();
        row.connect_active_notify(move |row| {
            // The local list is the source of truth while the dialog is open.
            let mut pinned = pinned.borrow_mut();
            if row.is_active() {
                if !pinned.contains(&app_id) {
                    pinned.push(app_id.clone());
                }
            } else {
                pinned.retain(|known| known != &app_id);
            }
            context.set("apps", serde_json::json!(pinned.clone()));
        });

        list.append(&row);
        toggles.push((app, row));
    }

    // Typing filters the list by name or id.
    let toggles = Rc::new(toggles);
    search.connect_search_changed({
        let toggles = toggles.clone();
        move |entry| {
            let needle = entry.text().to_lowercase();
            for (app, row) in toggles.iter() {
                let matches = needle.is_empty()
                    || app.display_name().to_lowercase().contains(&needle)
                    || app
                        .id()
                        .is_some_and(|id| id.to_lowercase().contains(&needle));
                row.set_visible(matches);
            }
        }
    });

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 6);
    holder.append(&search);
    holder.append(&list);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(340)
        .child(&holder)
        .build();
    scrolled.add_css_class("card");

    page.custom(&scrolled);
    page.spin(
        context,
        Spin::new("columns", "列数", config.columns as f64, 1.0, 8.0),
    );
    page.switch(context, "show_labels", "アプリ名を表示", config.show_labels);
    page.note("ここで選んだアプリがタイルに並びます。変更はすぐに保存されます。");
    Ok(page.finish())
}
