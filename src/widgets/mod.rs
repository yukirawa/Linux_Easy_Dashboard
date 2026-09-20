//! The widget catalogue.
//!
//! Adding a widget means adding one module with a [`WidgetDescriptor`] and
//! listing it in [`descriptors`]. The canvas only ever talks to widgets through
//! this interface, so a widget that fails to build or misbehaves can only ever
//! break its own tile.

pub mod analog_clock;
pub mod battery;
pub mod calendar;
pub mod clock;
pub mod cores;
pub mod cpu;
pub mod date;
pub mod disk;
pub mod diskio;
pub mod heading;
pub mod launcher;
pub mod load;
pub mod media;
pub mod memory;
pub mod network;
pub mod note;
pub mod plugin;
pub mod processes;
pub mod system;
pub mod systeminfo;
pub mod temperature;
pub mod weather;
pub mod world_clock;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use gtk::prelude::*;
use libadwaita::prelude::*;
use log::warn;
use serde::de::DeserializeOwned;

use crate::config::WidgetInstance;
use crate::ui::debounce::Debounce;

/// What a widget gets from the canvas: who it is, its saved settings, and a way
/// to write settings back (persisted immediately).
#[derive(Clone)]
pub struct WidgetContext {
    instance_id: String,
    kind: String,
    config: serde_json::Value,
    set_config: Rc<dyn Fn(serde_json::Value)>,
    get_config: Rc<dyn Fn() -> serde_json::Value>,
    set_hidden: Rc<dyn Fn(bool)>,
}

impl WidgetContext {
    pub fn new(
        instance_id: impl Into<String>,
        kind: impl Into<String>,
        config: serde_json::Value,
        set_config: Rc<dyn Fn(serde_json::Value)>,
        get_config: Rc<dyn Fn() -> serde_json::Value>,
        set_hidden: Rc<dyn Fn(bool)>,
    ) -> Self {
        Self {
            instance_id: instance_id.into(),
            kind: kind.into(),
            config,
            set_config,
            get_config,
            set_hidden,
        }
    }

    /// Reports whether this tile is worth showing right now.
    ///
    /// A widget with nothing to show — no media player running, an empty plugin
    /// source — can hide its tile. Hidden tiles keep their place on the canvas
    /// and reappear in edit mode, so they stay movable and removable.
    pub fn set_hidden(&self, hidden: bool) {
        (self.set_hidden)(hidden);
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Deserializes this instance's settings, falling back to the widget's
    /// defaults when the stored JSON is missing or was written by another
    /// version.
    pub fn config<T: DeserializeOwned + Default>(&self) -> T {
        decode(&self.kind, &self.config)
    }

    /// The same settings, as they are *now*.
    ///
    /// A tile is rebuilt whenever its settings change, so the value this context
    /// was built with can be out of date. Widgets that write back from a slow
    /// callback — a network reply — compare the two, so an answer about a place
    /// or a unit the user has already changed is dropped instead of resurrecting
    /// stale settings.
    pub fn current<T: DeserializeOwned + Default>(&self) -> T {
        decode(&self.kind, &(self.get_config)())
    }

    /// Sends a patch to the canvas, which merges it into the *live* settings and
    /// persists the result.
    ///
    /// Patches are merged on the canvas rather than here on purpose: a detail
    /// view can be open while other changes land, and merging into the value
    /// this context was built with would resurrect stale keys.
    pub fn update(&self, patch: serde_json::Value) {
        (self.set_config)(patch);
    }

    /// Convenience for the common `{"key": value}` patch.
    pub fn set(&self, key: &str, value: serde_json::Value) {
        let mut patch = serde_json::Map::new();
        patch.insert(key.to_owned(), value);
        self.update(serde_json::Value::Object(patch));
    }
}

/// Reads settings that may have been written by an older version, or be
/// missing entirely.
fn decode<T: DeserializeOwned + Default>(kind: &str, config: &serde_json::Value) -> T {
    if config.is_null() {
        return T::default();
    }
    match serde_json::from_value(config.clone()) {
        Ok(value) => value,
        Err(e) => {
            warn!("{kind} の設定を解釈できません: {e}");
            T::default()
        }
    }
}

/// A widget type as advertised to the canvas and the "add widget" picker.
pub struct WidgetDescriptor {
    /// Stable identifier stored in the layout file. Never rename it.
    pub kind: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    pub icon: &'static str,
    /// Grouping in the "add widget" picker.
    pub category: Category,
    pub default_size: (i32, i32),
    /// Fixed aspect ratio (width / height) for widgets that should not be
    /// stretched, e.g. a round gauge.
    pub aspect: Option<f64>,
    /// Builds the tile contents.
    pub build: fn(&WidgetContext) -> Result<gtk::Widget>,
    /// Builds the larger view shown when the tile is activated.
    pub detail: fn(&WidgetContext) -> Result<gtk::Widget>,
}

impl WidgetDescriptor {
    /// Smallest sensible tile size for this widget.
    pub fn min_size(&self) -> (i32, i32) {
        (self.default_size.0 / 2, self.default_size.1 / 2)
    }
}

/// Sections of the "add widget" picker, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    System,
    Time,
    Info,
    Apps,
}

impl Category {
    /// Every section, in the order the picker shows them.
    pub const ALL: [Self; 4] = [Self::System, Self::Time, Self::Info, Self::Apps];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "システム",
            Self::Time => "時刻と日付",
            Self::Info => "情報",
            Self::Apps => "アプリ",
        }
    }

    /// This section's widgets, in registry order.
    pub fn widgets(self) -> Vec<&'static WidgetDescriptor> {
        descriptors()
            .iter()
            .filter(|descriptor| descriptor.category == self)
            .collect()
    }
}

/// Every widget the application knows about.
///
/// The order is the order of the picker: grouped by category, and inside a
/// category roughly from "monitors the machine" to "decorates the dashboard".
/// There is a test for the grouping, because an interleaved list is what makes
/// the picker look cluttered.
pub fn descriptors() -> &'static [WidgetDescriptor] {
    static DESCRIPTORS: &[WidgetDescriptor] = &[
        // システム
        cpu::DESCRIPTOR,
        cores::DESCRIPTOR,
        memory::DESCRIPTOR,
        disk::DESCRIPTOR,
        diskio::DESCRIPTOR,
        network::DESCRIPTOR,
        load::DESCRIPTOR,
        temperature::DESCRIPTOR,
        battery::DESCRIPTOR,
        processes::DESCRIPTOR,
        system::DESCRIPTOR,
        // 時刻と日付
        clock::DESCRIPTOR,
        analog_clock::DESCRIPTOR,
        world_clock::DESCRIPTOR,
        calendar::DESCRIPTOR,
        date::DESCRIPTOR,
        // 情報
        weather::DESCRIPTOR,
        systeminfo::DESCRIPTOR,
        media::DESCRIPTOR,
        heading::DESCRIPTOR,
        note::DESCRIPTOR,
        // アプリ
        launcher::DESCRIPTOR,
    ];
    DESCRIPTORS
}

/// Looks up a widget kind.
///
/// The plugin kind is deliberately kept out of [`descriptors`]: a plugin tile
/// always belongs to one user written definition, so it is added from the
/// plugin section of the picker rather than from the catalogue itself.
pub fn find(kind: &str) -> Option<&'static WidgetDescriptor> {
    if kind == plugin::KIND {
        return Some(&plugin::DESCRIPTOR);
    }
    descriptors().iter().find(|d| d.kind == kind)
}

/// Builds the tile for `instance`, or a self explaining placeholder when the
/// widget is unknown or its constructor failed.
pub fn build_tile(
    instance: &WidgetInstance,
    set_config: Rc<dyn Fn(serde_json::Value)>,
    get_config: Rc<dyn Fn() -> serde_json::Value>,
    set_hidden: Rc<dyn Fn(bool)>,
) -> gtk::Widget {
    let context = WidgetContext::new(
        instance.id.clone(),
        instance.kind.clone(),
        instance.config.clone(),
        set_config,
        get_config,
        set_hidden,
    );

    let Some(descriptor) = find(&instance.kind) else {
        return error_tile(
            &instance.kind,
            "このウィジェットはこのバージョンでは利用できません",
        );
    };

    match (descriptor.build)(&context) {
        Ok(widget) => widget,
        Err(e) => {
            warn!("{} の生成に失敗しました: {e:#}", instance.kind);
            error_tile(descriptor.name, &format!("{e:#}"))
        }
    }
}

/// Placeholder shown in place of a widget that could not be created.
pub fn error_tile(name: &str, reason: &str) -> gtk::Widget {
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.set_pixel_size(24);
    icon.add_css_class("dim-label");

    let title = gtk::Label::new(Some(&format!("{name} を表示できません")));
    title.add_css_class("heading");
    title.set_wrap(true);

    let detail = gtk::Label::new(Some(reason));
    detail.add_css_class("caption");
    detail.add_css_class("dim-label");
    detail.set_wrap(true);

    let bx = gtk::Box::new(gtk::Orientation::Vertical, 6);
    bx.set_valign(gtk::Align::Center);
    bx.set_vexpand(true);
    bx.set_halign(gtk::Align::Center);
    bx.append(&icon);
    bx.append(&title);
    bx.append(&detail);
    bx.upcast()
}

/// Calls `refresh` every `seconds` while `anchor` is inside the widget tree.
///
/// This keeps timers from leaking: once a tile is removed from the canvas the
/// source stops itself as soon as it notices, without any explicit teardown.
pub fn tick_seconds(anchor: &impl IsA<gtk::Widget>, seconds: u32, refresh: impl Fn() + 'static) {
    refresh();

    let anchor: gtk::Widget = anchor.upcast_ref::<gtk::Widget>().clone();
    let started = Cell::new(false);
    glib::timeout_add_seconds_local(seconds.max(1), move || {
        let rooted = anchor.root().is_some();
        if !rooted && started.get() {
            return glib::ControlFlow::Break;
        }
        if rooted {
            started.set(true);
            refresh();
        }
        glib::ControlFlow::Continue
    });
}

/// Calls `refresh` every `millis` milliseconds while `anchor` is in the tree.
///
/// Used by the few widgets that need sub-second updates (the analog clock's
/// second hand); everything else uses [`tick_seconds`].
pub fn tick_millis(anchor: &impl IsA<gtk::Widget>, millis: u32, refresh: impl Fn() + 'static) {
    refresh();

    let anchor: gtk::Widget = anchor.upcast_ref::<gtk::Widget>().clone();
    let started = Cell::new(false);
    glib::timeout_add_local(
        std::time::Duration::from_millis(u64::from(millis.max(16))),
        move || {
            let rooted = anchor.root().is_some();
            if !rooted && started.get() {
                return glib::ControlFlow::Break;
            }
            if rooted {
                started.set(true);
                refresh();
            }
            glib::ControlFlow::Continue
        },
    );
}

/// The standard tile header: an icon and a title on the left, a value on the
/// right. Shared so every widget reads the same way.
#[derive(Clone)]
pub struct TileHeader {
    pub root: gtk::Box,
    pub value: gtk::Label,
}

impl TileHeader {
    pub fn new(icon: &str, title: &str) -> Self {
        let image = gtk::Image::from_icon_name(icon);
        image.add_css_class("dim-label");
        image.set_pixel_size(14);

        let label = gtk::Label::new(Some(title));
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        label.set_xalign(0.0);
        label.set_hexpand(true);

        let value = gtk::Label::new(None);
        value.add_css_class("edm-mono");
        value.add_css_class("caption");
        value.set_xalign(1.0);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.append(&image);
        root.append(&label);
        root.append(&value);

        Self { root, value }
    }

    pub fn set_value(&self, text: &str) {
        if self.value.label() != text {
            self.value.set_label(text);
        }
    }
}

/// A large value with a caption underneath: the usual tile readout.
#[derive(Clone)]
pub struct Readout {
    pub root: gtk::Box,
    pub value: gtk::Label,
    pub caption: gtk::Label,
}

impl Readout {
    pub fn new() -> Self {
        let value = gtk::Label::new(None);
        value.add_css_class("edm-readout");
        value.set_xalign(0.5);

        let caption = gtk::Label::new(None);
        caption.add_css_class("caption");
        caption.add_css_class("dim-label");
        caption.set_xalign(0.5);
        caption.set_wrap(true);
        caption.set_justify(gtk::Justification::Center);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_valign(gtk::Align::Center);
        root.append(&value);
        root.append(&caption);

        Self {
            root,
            value,
            caption,
        }
    }

    pub fn set(&self, value: &str, caption: &str) {
        if self.value.label() != value {
            self.value.set_label(value);
        }
        if self.caption.label() != caption {
            self.caption.set_label(caption);
        }
    }
}

impl Default for Readout {
    fn default() -> Self {
        Self::new()
    }
}

/// A numeric settings row.
pub struct Spin {
    key: &'static str,
    title: &'static str,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
}

impl Spin {
    pub fn new(key: &'static str, title: &'static str, value: f64, min: f64, max: f64) -> Self {
        Self {
            key,
            title,
            value,
            min,
            max,
            step: 1.0,
        }
    }
}

/// How long typing must pause before a text row writes its value back.
const TEXT_DEBOUNCE: Duration = Duration::from_millis(600);

/// Turns a text row's contents into a settings patch.
type Patch = Rc<dyn Fn(&str) -> serde_json::Value>;

/// Writes a text row's value back to the canvas, coalescing keystrokes.
struct TextWriter {
    context: WidgetContext,
    patch: Patch,
    debounce: Debounce,
}

impl TextWriter {
    /// Queues the value; only the last keystroke of a burst is written.
    fn later(&self, text: &str) {
        let context = self.context.clone();
        let patch = Rc::clone(&self.patch);
        let text = text.to_owned();
        self.debounce.schedule(move || context.update(patch(&text)));
    }

    /// Writes the value now, e.g. when the user presses Enter.
    fn now(&self, text: &str) {
        self.debounce.cancel();
        self.context.update((self.patch)(text));
    }
}

/// Builds an entry row whose contents are written back through `patch`.
///
/// An entry row saves while the user types — coalesced by a short debounce —
/// and immediately on Enter. It deliberately does *not* wait for
/// `AdwEntryRow`'s `apply` signal: that one only fires for the row's optional
/// apply button, which is hidden by default, so relying on it silently throws
/// every edit away.
pub fn text_row(
    context: &WidgetContext,
    title: &str,
    value: &str,
    patch: impl Fn(&str) -> serde_json::Value + 'static,
) -> libadwaita::EntryRow {
    let row = libadwaita::EntryRow::builder()
        .title(title)
        .text(value)
        .build();
    let writer = Rc::new(TextWriter {
        context: context.clone(),
        patch: Rc::new(patch),
        debounce: Debounce::new(TEXT_DEBOUNCE),
    });

    row.connect_changed({
        let writer = Rc::clone(&writer);
        move |row| writer.later(&row.text())
    });
    row.connect_entry_activated(move |row| writer.now(&row.text()));

    row
}

/// Splits a comma separated line into trimmed, non-empty items.
pub fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A single key patch, e.g. `{"title": "..."}`.
fn patch_of(key: &str, value: serde_json::Value) -> serde_json::Value {
    let mut patch = serde_json::Map::new();
    patch.insert(key.to_owned(), value);
    serde_json::Value::Object(patch)
}

/// A scrollable detail page: read-only facts on top, settings below.
///
/// Detail views are almost always "show the numbers, offer a few switches", so
/// they share this layout instead of repeating the plumbing.
pub struct DetailPage {
    root: gtk::ScrolledWindow,
    content: gtk::Box,
    facts: libadwaita::PreferencesGroup,
    settings: libadwaita::PreferencesGroup,
}

impl DetailPage {
    pub fn new(facts_title: &str, settings_title: &str) -> Self {
        let facts = libadwaita::PreferencesGroup::new();
        facts.set_title(facts_title);

        let settings = libadwaita::PreferencesGroup::new();
        settings.set_title(settings_title);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.set_margin_top(12);
        content.set_margin_bottom(24);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.append(&facts);
        content.append(&settings);

        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&content)
            .build();

        Self {
            root,
            content,
            facts,
            settings,
        }
    }

    /// Adds a read-only row.
    pub fn fact(&self, title: &str, subtitle: &str) {
        let row = libadwaita::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .build();
        self.facts.add(&row);
    }

    /// Adds a row the widget built itself.
    pub fn custom(&self, widget: &impl IsA<gtk::Widget>) {
        self.content.append(widget);
    }

    /// Adds a row the widget built itself to the facts group.
    pub fn fact_row(&self, row: &impl IsA<gtk::Widget>) {
        self.facts.add(row);
    }

    /// Adds a switch that persists `key` whenever it is toggled.
    pub fn switch(&self, context: &WidgetContext, key: &'static str, title: &str, active: bool) {
        let row = libadwaita::SwitchRow::builder()
            .title(title)
            .active(active)
            .build();
        row.set_tooltip_text(Some("変更はすぐに保存されます"));
        let context = context.clone();
        row.connect_active_notify(move |row| {
            context.set(key, serde_json::Value::Bool(row.is_active()));
        });
        self.settings.add(&row);
    }

    /// Adds a numeric row that persists `key`.
    pub fn spin(&self, context: &WidgetContext, spin: Spin) {
        let adjustment = gtk::Adjustment::new(
            spin.value,
            spin.min,
            spin.max,
            spin.step,
            spin.step * 5.0,
            0.0,
        );
        let row = libadwaita::SpinRow::builder()
            .title(spin.title)
            .adjustment(&adjustment)
            .build();
        let context = context.clone();
        let key = spin.key;
        row.connect_value_notify(move |row| {
            // Stored as an integer: the widgets read these back into `usize`.
            context.set(key, serde_json::json!(row.value().round() as i64));
        });
        self.settings.add(&row);
    }

    /// Adds a text row that persists whatever `patch` derives from its content.
    pub fn text(
        &self,
        context: &WidgetContext,
        title: &str,
        value: &str,
        patch: impl Fn(&str) -> serde_json::Value + 'static,
    ) {
        self.settings.add(&text_row(context, title, value, patch));
    }

    /// Adds a row that persists one string setting under `key`.
    pub fn entry(&self, context: &WidgetContext, key: &'static str, title: &str, value: &str) {
        self.text(context, title, value, move |text| {
            patch_of(key, serde_json::Value::String(text.to_owned()))
        });
    }

    /// Adds a row editing a list of strings: written as one comma separated
    /// line, stored as a JSON array.
    pub fn list(&self, context: &WidgetContext, key: &'static str, title: &str, values: &[String]) {
        self.text(context, title, &values.join(", "), move |text| {
            patch_of(key, serde_json::json!(split_list(text)))
        });
    }

    /// Adds a quiet explanation, e.g. why something is unavailable.
    pub fn note(&self, text: &str) {
        let label = gtk::Label::new(Some(text));
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        label.set_wrap(true);
        label.set_xalign(0.0);
        self.content.append(&label);
    }

    pub fn finish(self) -> gtk::Widget {
        self.root.upcast()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn comma_separated_lists_tolerate_loose_input() {
        assert_eq!(split_list("Asia/Tokyo, UTC"), ["Asia/Tokyo", "UTC"]);
        assert_eq!(split_list("  a ,, b  "), ["a", "b"]);
        assert_eq!(split_list(""), Vec::<String>::new());
        assert_eq!(split_list(" , "), Vec::<String>::new());
    }

    #[test]
    fn patches_only_touch_the_named_key() {
        let patch = patch_of("title", serde_json::json!("hello"));
        assert_eq!(patch, serde_json::json!({ "title": "hello" }));
    }

    /// The picker groups consecutive runs by category, so the registry has to
    /// keep each category in one run.
    #[test]
    fn the_registry_is_grouped_by_category() {
        let mut sections: Vec<Category> = Vec::new();
        for descriptor in descriptors() {
            if sections.last() != Some(&descriptor.category) {
                assert!(
                    !sections.contains(&descriptor.category),
                    "{} が離れた場所にあります",
                    descriptor.category.label()
                );
                sections.push(descriptor.category);
            }
        }
        assert_eq!(sections.as_slice(), &Category::ALL);
    }

    #[test]
    fn every_category_has_widgets_and_every_kind_is_unique() {
        for category in Category::ALL {
            assert!(
                !category.widgets().is_empty(),
                "{} が空です",
                category.label()
            );
        }

        let mut kinds = HashSet::new();
        for descriptor in descriptors() {
            assert!(
                kinds.insert(descriptor.kind),
                "{} が重複しています",
                descriptor.kind
            );
            assert!(!descriptor.name.is_empty());
            assert!(!descriptor.summary.is_empty());
        }
        assert_eq!(kinds.len(), descriptors().len());
    }
}
