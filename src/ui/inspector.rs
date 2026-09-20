//! Detail view for a single widget: the same widget built at a comfortable
//! size, presented in a libadwaita dialog.

use anyhow::{Result, anyhow};
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::widgets::{self, WidgetContext};

/// Presents the detail view of `kind` on top of `parent`.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    kind: &str,
    context: &WidgetContext,
) -> Result<adw::Dialog> {
    let descriptor = widgets::find(kind).ok_or_else(|| anyhow!("未知のウィジェット: {kind}"))?;
    let content = (descriptor.detail)(context)?;

    let toolbar = adw::ToolbarView::new();
    toolbar.set_content(Some(&content));

    let dialog = adw::Dialog::builder()
        .title(descriptor.name)
        .content_width(460)
        .content_height(540)
        .child(&toolbar)
        .build();

    toolbar.add_top_bar(&build_header(descriptor, &dialog));
    dialog.present(Some(parent));

    Ok(dialog)
}

fn build_header(descriptor: &widgets::WidgetDescriptor, dialog: &adw::Dialog) -> adw::HeaderBar {
    let title = gtk::Label::new(Some(descriptor.name));
    title.add_css_class("heading");

    let subtitle = gtk::Label::new(Some(descriptor.summary));
    subtitle.add_css_class("caption");
    subtitle.add_css_class("dim-label");

    let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
    titles.set_valign(gtk::Align::Center);
    titles.append(&title);
    titles.append(&subtitle);

    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("flat");
    close.set_tooltip_text(Some("閉じる"));
    let weak = dialog.downgrade();
    close.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });

    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    header.set_title_widget(Some(&titles));
    header.pack_end(&close);
    header
}
