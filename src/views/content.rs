//! Settings-only views: heading, note, launcher.

use anyhow::Result;
use gtk::prelude::*;

use crate::plugin::{Plugin, View};
use crate::widgets::WidgetContext;

pub fn heading(definition: &Plugin) -> Result<gtk::Widget> {
    let (title, subtitle, accent) = match &definition.view {
        View::Heading {
            title,
            subtitle,
            accent,
        } => (title.clone(), subtitle.clone(), *accent),
        _ => unreachable!("heading called with the wrong view"),
    };

    let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bar.set_size_request(-1, 4);
    bar.add_css_class("accent");
    bar.set_visible(accent);

    let title_label = gtk::Label::new(Some(if title.trim().is_empty() {
        &definition.name
    } else {
        title.trim()
    }));
    title_label.add_css_class("title-1");
    title_label.set_xalign(0.0);
    title_label.set_wrap(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_valign(gtk::Align::Center);
    root.set_vexpand(true);
    if accent {
        root.append(&bar);
    }
    root.append(&title_label);
    if !subtitle.trim().is_empty() {
        let sub = gtk::Label::new(Some(subtitle.trim()));
        sub.add_css_class("heading");
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        root.append(&sub);
    }
    Ok(root.upcast())
}

pub fn note(definition: &Plugin, _context: &WidgetContext) -> Result<gtk::Widget> {
    let (title, text) = match &definition.view {
        View::Note { title, text } => (title.clone(), text.clone()),
        _ => unreachable!("note called with the wrong view"),
    };
    // Instance overrides are already applied to `definition` by the caller.

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    if !title.trim().is_empty() {
        let head = gtk::Label::new(Some(title.trim()));
        head.add_css_class("heading");
        head.set_xalign(0.0);
        head.set_ellipsize(gtk::pango::EllipsizeMode::End);
        root.append(&head);
    }
    let label = gtk::Label::new(Some(if text.trim().is_empty() {
        "（空）"
    } else {
        text.trim()
    }));
    label.set_xalign(0.0);
    label.set_yalign(0.0);
    label.set_wrap(true);
    label.set_selectable(true);
    label.set_vexpand(true);
    if text.trim().is_empty() {
        label.add_css_class("dim-label");
    }
    root.append(&label);
    Ok(root.upcast())
}

pub fn launcher(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let (apps, columns, show_labels) = match &definition.view {
        View::Launcher {
            apps,
            columns,
            show_labels,
        } => (crate::views::csv(apps), *columns, *show_labels),
        _ => unreachable!("launcher called with the wrong view"),
    };
    let _ = context;

    let installed = gtk::gio::AppInfo::all();

    let grid = gtk::FlowBox::new();
    grid.set_max_children_per_line(columns as u32);
    grid.set_min_children_per_line(1);
    grid.set_selection_mode(gtk::SelectionMode::None);
    grid.set_homogeneous(true);
    grid.set_valign(gtk::Align::Center);
    grid.set_vexpand(true);

    for id in apps {
        let Some(app) = installed
            .iter()
            .find(|app| app.id().as_deref() == Some(id.as_str()))
        else {
            log::warn!("アプリ {id} が見つかりません");
            continue;
        };
        let button = gtk::Button::new();
        button.set_hexpand(true);
        button.set_tooltip_text(Some(app.display_name().as_str()));
        button.add_css_class("flat");

        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
        box_.set_halign(gtk::Align::Center);
        if let Some(icon) = app.icon() {
            let image = gtk::Image::from_gicon(&icon);
            image.set_pixel_size(32);
            box_.append(&image);
        }
        if show_labels {
            let label = gtk::Label::new(Some(app.display_name().as_str()));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(12);
            box_.append(&label);
        }
        button.set_child(Some(&box_));

        let for_launch = app.clone();
        button.connect_clicked(move |_| {
            if let Err(e) = for_launch.launch(&[], None::<&gtk::gio::AppLaunchContext>) {
                log::warn!("起動できません: {e}");
            }
        });
        grid.insert(&button, -1);
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&grid)
        .build();
    Ok(scrolled.upcast())
}


