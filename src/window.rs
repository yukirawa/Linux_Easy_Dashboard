//! The main window: header bar, toolbar, canvas and the persistence loop.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::gio;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use log::{debug, warn};

use crate::canvas::Canvas;
use crate::config::{AppConfig, CanvasSettings, ConfigStore, WindowSettings};
use crate::ui;
use crate::ui::debounce::Debounce;
use crate::widgets::{self, Category};

/// Delay between the last change and writing the layout to disk. Long enough
/// to coalesce a drag, short enough to survive a crash.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(700);

pub struct DashboardWindow {
    window: adw::ApplicationWindow,
    canvas: Canvas,
    toast: adw::ToastOverlay,
    /// Shown while the dashboard has no widgets at all.
    empty_state: gtk::Widget,
    /// Hint bar, only visible while editing.
    hint_bar: gtk::Widget,
    /// Source of truth for edit mode; the header button and the shortcut both
    /// drive it.
    edit_button: gtk::ToggleButton,
    /// End area of the header bar, filled once the window exists.
    header_actions: gtk::Box,
    /// The "add widget" picker. Kept around so a plugin reload can refill it.
    add_button: AddButton,
    store: ConfigStore,
    canvas_settings: RefCell<CanvasSettings>,
    geometry: RefCell<WindowSettings>,
    /// Last payload written to disk, so an unchanged layout is never rewritten.
    saved: RefCell<String>,
    save: Debounce,
}

impl DashboardWindow {
    pub fn new(app: &adw::Application, store: ConfigStore, config: AppConfig) -> Rc<Self> {
        let canvas = Canvas::new();
        canvas.set_grid(config.canvas.grid);
        canvas.set_snap_enabled(config.canvas.snap);
        canvas.set_show_grid(config.canvas.show_grid);
        for instance in &config.widgets {
            canvas.add_instance(instance);
        }

        let toast = adw::ToastOverlay::new();
        let empty_state = build_empty_state();
        let hint_bar = build_hint_bar();
        let chrome = build_header();
        let add_button = AddButton::new();

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(crate::application::APP_NAME)
            .default_width(config.window.width.max(480))
            .default_height(config.window.height.max(360))
            .build();

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&chrome.header);
        toolbar.add_bottom_bar(&hint_bar);
        toolbar.set_content(Some(&build_body(&canvas, &empty_state)));
        toast.set_child(Some(&toolbar));
        window.set_content(Some(&toast));

        let this = Rc::new(Self {
            window,
            canvas,
            toast,
            empty_state,
            hint_bar,
            edit_button: chrome.edit_button,
            header_actions: chrome.actions,
            add_button,
            store,
            canvas_settings: RefCell::new(config.canvas.clone()),
            geometry: RefCell::new(config.window.clone()),
            saved: RefCell::new(String::new()),
            save: Debounce::new(SAVE_DEBOUNCE),
        });

        this.install_signals();
        this.sync_empty_state();

        if config.window.maximized {
            this.window.maximize();
        }
        // Start from a known baseline so the first change is detected.
        *this.saved.borrow_mut() = this.payload();

        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// The underlying toplevel, e.g. as a parent for dialogs.
    pub fn app_window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    /// The dashboard canvas.
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// Registers every callback that needs the whole window.
    fn install_signals(self: &Rc<Self>) {
        self.canvas.connect_changed({
            let weak = Rc::downgrade(self);
            move || {
                if let Some(this) = weak.upgrade() {
                    this.sync_empty_state();
                    this.schedule_save();
                }
            }
        });

        self.canvas.connect_activate({
            let weak = Rc::downgrade(self);
            move |id| {
                if let Some(this) = weak.upgrade() {
                    this.open_detail(id);
                }
            }
        });

        self.window.connect_close_request({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(this) = weak.upgrade() {
                    this.save_now();
                }
                glib::Propagation::Proceed
            }
        });

        // Edit mode: the toggle button owns the state.
        self.edit_button.connect_toggled({
            let weak = Rc::downgrade(self);
            move |button| {
                if let Some(this) = weak.upgrade() {
                    let enabled = button.is_active();
                    this.canvas.set_edit_mode(enabled);
                    this.hint_bar.set_visible(enabled);
                }
            }
        });

        // The shortcut and the menu route through a plain window action so the
        // accel only has to be declared in one place.
        let shortcut = gio::SimpleAction::new("toggle-edit", None);
        shortcut.connect_activate({
            let weak = Rc::downgrade(self);
            move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.edit_button.set_active(!this.edit_button.is_active());
                }
            }
        });
        self.window.add_action(&shortcut);

        self.install_add_button();
        self.install_plugin_actions();
        self.install_toggles();

        let remove = gio::SimpleAction::new("remove-selected", None);
        remove.connect_activate({
            let weak = Rc::downgrade(self);
            move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.canvas.remove_selected();
                }
            }
        });
        self.window.add_action(&remove);

        let reset = gio::SimpleAction::new("reset-layout", None);
        reset.connect_activate({
            let weak = Rc::downgrade(self);
            move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.confirm_reset();
                }
            }
        });
        self.window.add_action(&reset);
    }

    fn install_add_button(self: &Rc<Self>) {
        // The picker was built before the window existed, so its choice is
        // delivered here.
        self.add_button.set_handler({
            let weak = Rc::downgrade(self);
            move |pick| {
                if let Some(this) = weak.upgrade() {
                    this.add_pick(pick);
                }
            }
        });
        self.header_actions.prepend(&self.add_button.button);
    }

    /// Plugin housekeeping: re-read the directory, and open it next to the app.
    fn install_plugin_actions(self: &Rc<Self>) {
        let reload = gio::SimpleAction::new("reload-plugins", None);
        reload.connect_activate({
            let weak = Rc::downgrade(self);
            move |_, _| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let count = crate::plugin::reload();
                this.add_button.fill();
                // Definitions changed, so the tiles drawn from them are stale.
                this.canvas.refresh_kind(crate::widgets::plugin::KIND);
                this.show_toast(&format!("プラグインを {count} 件読み込みました"));
            }
        });
        self.window.add_action(&reload);

        let open = gio::SimpleAction::new("open-plugins", None);
        open.connect_activate(|_, _| {
            let directory = crate::plugin::directory();
            let uri = format!("file://{}", directory.display());
            if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE)
            {
                warn!("{} を開けません: {e}", directory.display());
            }
        });
        self.window.add_action(&open);
    }

    /// Adds whatever the picker was asked for and switches to edit mode so it
    /// can be adjusted at once.
    fn add_pick(self: &Rc<Self>, pick: Pick) {
        let added = match pick {
            Pick::Builtin(kind) => self.canvas.add_kind(kind),
            Pick::Plugin(id) => {
                // The size belongs to the definition, not to the generic widget.
                let size = crate::plugin::find(&id)
                    .map(|loaded| (loaded.plugin.size.0, loaded.plugin.size.1));
                self.canvas.add_kind_with(
                    crate::widgets::plugin::KIND,
                    serde_json::json!({ "plugin": id }),
                    size,
                )
            }
        };
        if added.is_some() {
            self.edit_button.set_active(true);
        }
    }

    /// Display toggles, persisted with the rest of the layout.
    fn install_toggles(self: &Rc<Self>) {
        let grid = gio::SimpleAction::new_stateful(
            "show-grid",
            None,
            &self.canvas_settings.borrow().show_grid.to_variant(),
        );
        grid.connect_activate({
            let weak = Rc::downgrade(self);
            move |action, _| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let enabled = state_of(action);
                action.set_state(&enabled.to_variant());
                this.canvas.set_show_grid(enabled);
                this.canvas_settings.borrow_mut().show_grid = enabled;
                this.schedule_save();
            }
        });
        self.window.add_action(&grid);

        let snap = gio::SimpleAction::new_stateful(
            "snap",
            None,
            &self.canvas_settings.borrow().snap.to_variant(),
        );
        snap.connect_activate({
            let weak = Rc::downgrade(self);
            move |action, _| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let enabled = state_of(action);
                action.set_state(&enabled.to_variant());
                this.canvas.set_snap_enabled(enabled);
                this.canvas_settings.borrow_mut().snap = enabled;
                this.schedule_save();
            }
        });
        self.window.add_action(&snap);
    }

    /// Adds a widget and switches to edit mode so it can be adjusted at once.
    fn open_detail(&self, id: &str) {
        let Some(context) = self.canvas.widget_context(id) else {
            return;
        };
        let Some(kind) = self.canvas.widget_kind(id) else {
            return;
        };
        if let Err(e) = ui::inspector::present(&self.window, &kind, &context) {
            warn!("詳細を開けません: {e:#}");
            self.show_toast(&format!("詳細を開けません: {e}"));
        }
    }

    fn confirm_reset(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::builder()
            .heading("レイアウトを初期化しますか？")
            .body("配置したウィジェットと設定を破棄して、初期状態に戻します。")
            .build();
        dialog.add_response("cancel", "キャンセル");
        dialog.add_response("reset", "初期化");
        dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        dialog.connect_response(None, {
            let weak = Rc::downgrade(self);
            move |_, response| {
                if response != "reset" {
                    return;
                }
                let Some(this) = weak.upgrade() else {
                    return;
                };
                this.canvas.clear();
                for instance in AppConfig::starter().widgets {
                    this.canvas.add_instance(&instance);
                }
                this.sync_empty_state();
                this.schedule_save();
                this.show_toast("レイアウトを初期化しました");
            }
        });
        dialog.present(Some(&self.window));
    }

    fn sync_empty_state(&self) {
        self.empty_state
            .set_visible(self.canvas.widget_count() == 0);
    }

    fn show_toast(&self, message: &str) {
        self.toast.add_toast(adw::Toast::new(message));
    }

    /// Shows a transient message above the dashboard.
    pub fn notify(&self, message: &str) {
        self.show_toast(message);
    }

    // -- persistence ------------------------------------------------------

    /// Current layout plus window geometry, ready to be written.
    fn snapshot(&self) -> AppConfig {
        AppConfig {
            window: self.current_geometry(),
            canvas: self.canvas_settings.borrow().clone(),
            widgets: self.canvas.instances(),
            ..AppConfig::default()
        }
    }

    fn current_geometry(&self) -> WindowSettings {
        let mut settings = self.geometry.borrow().clone();
        settings.maximized = self.window.is_maximized();
        if self.window.is_realized() && !settings.maximized && !self.window.is_fullscreen() {
            let (width, height) = (self.window.width(), self.window.height());
            if width > 0 && height > 0 {
                settings.width = width;
                settings.height = height;
            }
        }
        settings
    }

    fn payload(&self) -> String {
        serde_json::to_string(&self.snapshot()).unwrap_or_default()
    }

    fn schedule_save(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.save.schedule(move || {
            if let Some(this) = weak.upgrade() {
                this.save_now();
            }
        });
    }

    fn save_now(&self) {
        let config = self.snapshot();
        let payload = match serde_json::to_string(&config) {
            Ok(payload) => payload,
            Err(e) => {
                warn!("レイアウトを直列化できません: {e}");
                return;
            }
        };
        if *self.saved.borrow() == payload && self.store.path().exists() {
            return;
        }

        match self.store.save(&config) {
            Ok(()) => {
                debug!("レイアウトを保存しました: {}", self.store.path().display());
                *self.saved.borrow_mut() = payload;
            }
            Err(e) => {
                warn!("レイアウトを保存できません: {e:#}");
                self.show_toast(&format!("保存できません: {e}"));
            }
        }
    }
}

fn state_of(action: &gio::SimpleAction) -> bool {
    action
        .state()
        .and_then(|state| state.get::<bool>())
        .unwrap_or(false)
}

/// The header bar plus the pieces the window needs to keep a handle on.
struct Chrome {
    header: adw::HeaderBar,
    edit_button: gtk::ToggleButton,
    /// Right hand side of the header, so the add button can be inserted in
    /// front of the primary menu once the window exists.
    actions: gtk::Box,
}

fn build_header() -> Chrome {
    let edit_button = gtk::ToggleButton::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("編集モード (Ctrl+E)")
        .build();
    edit_button.add_css_class("flat");

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.append(&build_menu_button());

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(
        crate::application::APP_NAME,
        "ダッシュボード",
    )));
    header.pack_start(&edit_button);
    header.pack_end(&actions);

    Chrome {
        header,
        edit_button,
        actions,
    }
}

fn build_body(canvas: &Canvas, empty_state: &gtk::Widget) -> gtk::Overlay {
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_width(false)
        .propagate_natural_height(false)
        .hexpand(true)
        .vexpand(true)
        .child(canvas)
        .build();

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scrolled));
    overlay.add_overlay(empty_state);
    overlay
}

fn build_menu_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();

    let canvas_menu = gio::Menu::new();
    canvas_menu.append(Some("グリッドを表示"), Some("win.show-grid"));
    canvas_menu.append(Some("スナップ"), Some("win.snap"));
    canvas_menu.append(
        Some("選択したウィジェットを削除"),
        Some("win.remove-selected"),
    );
    menu.append_submenu(Some("キャンバス"), &canvas_menu);

    let plugin_menu = gio::Menu::new();
    plugin_menu.append(Some("プラグインを再読み込み"), Some("win.reload-plugins"));
    plugin_menu.append(Some("プラグインのフォルダを開く"), Some("win.open-plugins"));
    menu.append_submenu(Some("プラグイン"), &plugin_menu);

    menu.append(Some("レイアウトを初期化"), Some("win.reset-layout"));
    menu.append(Some("バージョン情報"), Some("app.about"));
    menu.append(Some("終了"), Some("app.quit"));

    let button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("メニュー")
        .build();
    button.add_css_class("flat");
    button.set_menu_model(Some(&menu));
    button
}

/// Set once the window exists; before that the picker has nowhere to deliver to.
type PickHandler = Rc<RefCell<Option<Box<dyn Fn(Pick)>>>>;

/// What the picker was asked to add.
enum Pick {
    /// A widget from the built-in catalogue.
    Builtin(&'static str),
    /// A user written plugin, by definition id.
    Plugin(String),
}

/// One section of the picker: its heading and the rows below it.
struct Section {
    heading: gtk::ListBoxRow,
    /// Row plus the text the search matches it against.
    rows: Vec<(adw::ActionRow, String)>,
}

/// The "add widget" button: a searchable catalogue, grouped by category, with
/// the plugin directory as one more section.
///
/// It is built before the window exists, so the handler that actually adds
/// something is wired up later with [`AddButton::set_handler`].
struct AddButton {
    button: gtk::MenuButton,
    list: gtk::ListBox,
    sections: Rc<RefCell<Vec<Section>>>,
    handler: PickHandler,
}

impl AddButton {
    fn new() -> Self {
        let popover = gtk::Popover::new();
        popover.add_css_class("menu");
        popover.set_size_request(340, -1);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("ウィジェットを検索"));
        search.set_margin_top(6);
        search.set_margin_bottom(6);
        search.set_margin_start(6);
        search.set_margin_end(6);

        let list = gtk::ListBox::new();
        list.add_css_class("edm-picker");
        list.set_selection_mode(gtk::SelectionMode::None);

        let sections: Rc<RefCell<Vec<Section>>> = Rc::new(RefCell::new(Vec::new()));
        let handler: PickHandler = Rc::new(RefCell::new(None));

        // Filtering hides rows and, with them, the headings of empty sections.
        search.connect_search_changed({
            let sections = Rc::clone(&sections);
            move |entry| {
                let needle = entry.text().to_lowercase();
                for section in sections.borrow().iter() {
                    let mut shown = 0;
                    for (row, haystack) in &section.rows {
                        let visible = needle.is_empty() || haystack.contains(&needle);
                        row.set_visible(visible);
                        shown += usize::from(visible);
                    }
                    section.heading.set_visible(shown > 0);
                }
            }
        });

        // The row carries what it adds, so the handler never has to be rewired
        // when the catalogue is filled again.
        list.connect_row_activated({
            let handler = Rc::clone(&handler);
            move |_, row| {
                let name = row.widget_name().to_string();
                let pick = match name.strip_prefix("plugin:") {
                    Some(id) => Pick::Plugin(id.to_owned()),
                    None => match widgets::find(&name) {
                        Some(descriptor) => Pick::Builtin(descriptor.kind),
                        None => return,
                    },
                };
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(pick);
                }
            }
        });

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .max_content_height(420)
            .propagate_natural_height(true)
            .child(&list)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&search);
        content.append(&scrolled);
        popover.set_child(Some(&content));

        // Opening the picker should mean "start typing", not "scroll and look".
        popover.connect_show(move |_| {
            search.set_text("");
            scrolled.vadjustment().set_value(0.0);
            let search = search.clone();
            glib::idle_add_local_once(move || {
                search.grab_focus();
            });
        });

        let button = gtk::MenuButton::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("ウィジェットを追加")
            .build();
        button.add_css_class("flat");
        button.set_popover(Some(&popover));

        let this = Self {
            button,
            list,
            sections,
            handler,
        };
        this.fill();
        this
    }

    fn set_handler(&self, handler: impl Fn(Pick) + 'static) {
        *self.handler.borrow_mut() = Some(Box::new(handler));
    }

    /// (Re)builds the catalogue. Called again after a plugin reload.
    fn fill(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let mut sections = Vec::new();
        for category in Category::ALL {
            let rows = category
                .widgets()
                .into_iter()
                .map(|descriptor| {
                    row(
                        descriptor.name,
                        descriptor.summary,
                        descriptor.icon,
                        descriptor.kind,
                    )
                })
                .collect();
            sections.push(Section {
                heading: section_header(category.label()),
                rows,
            });
        }

        let plugins = crate::plugin::catalogue();
        if !plugins.is_empty() {
            let rows = plugins
                .iter()
                .map(|loaded| {
                    row(
                        &loaded.plugin.name,
                        &loaded.plugin.summary,
                        &loaded.plugin.icon,
                        &format!("plugin:{}", loaded.plugin.id),
                    )
                })
                .collect();
            sections.push(Section {
                heading: section_header("プラグイン"),
                rows,
            });
        }

        for section in &sections {
            self.list.append(&section.heading);
            for (row, _) in &section.rows {
                self.list.append(row);
            }
        }
        *self.sections.borrow_mut() = sections;
    }
}

/// One catalogue row: what it is called, what it does and what it adds.
fn row(name: &str, summary: &str, icon: &str, key: &str) -> (adw::ActionRow, String) {
    let row = adw::ActionRow::builder()
        .title(name)
        .subtitle(summary)
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    row.set_widget_name(key);
    let haystack = format!("{name} {summary} {key}").to_lowercase();
    (row, haystack)
}

/// A non-selectable heading inside the picker.
fn section_header(title: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::new(Some(title));
    label.add_css_class("edm-picker-section");
    label.set_xalign(0.0);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&label));
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_can_target(false);
    row
}

fn build_empty_state() -> gtk::Widget {
    let icon = gtk::Image::from_icon_name("view-grid-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("dim-label");

    let title = gtk::Label::new(Some("ウィジェットがありません"));
    title.add_css_class("title-2");

    let subtitle = gtk::Label::new(Some("ヘッダーの + から追加してください"));
    subtitle.add_css_class("dim-label");
    subtitle.set_wrap(true);
    subtitle.set_justify(gtk::Justification::Center);

    let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
    bx.set_valign(gtk::Align::Center);
    bx.set_halign(gtk::Align::Center);
    // Purely decorative: never swallow clicks meant for the canvas.
    bx.set_can_target(false);
    bx.append(&icon);
    bx.append(&title);
    bx.append(&subtitle);
    bx.upcast()
}

fn build_hint_bar() -> gtk::Widget {
    let label = gtk::Label::new(Some(
        "ドラッグで移動 · 角のグリップでリサイズ · Shift+矢印で大きく移動 · Delete で削除",
    ));
    label.add_css_class("caption");
    label.add_css_class("dim-label");

    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bx.add_css_class("edm-hint-bar");
    bx.set_halign(gtk::Align::Center);
    bx.set_margin_top(8);
    bx.set_margin_bottom(8);
    bx.append(&label);
    bx.set_visible(false);
    bx.upcast()
}
