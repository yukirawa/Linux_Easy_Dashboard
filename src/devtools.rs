//! Development aids. Not part of the user facing feature set.
//!
//! Three environment variables are understood:
//!
//! * `EDM_SCREENSHOT=/path/to.png` logs the tile geometry, checks that every
//!   widget's detail view builds, renders the window to a PNG and closes it.
//! * `EDM_DEV_DRAG="x,y,dx,dy"` switches to edit mode and drags the tile under
//!   `(x, y)` by `(dx, dy)`, which exercises move/snap without a pointer.
//! * `EDM_DEV_GALLERY=<page>` replaces the layout with one page of the widget
//!   catalogue, which is how the whole set gets smoke tested.
//! * `EDM_DEV_PATCH="kind:key=json"` sends a settings patch through the same
//!   path the detail view uses, e.g. `clock:hour24=false`.
//! * `EDM_DEV_PLUGIN="id,id"` adds one tile per plugin definition, which is how
//!   user written widgets get looked at (they are not in the gallery).
//! * `EDM_DEV_DETAIL=kind` opens that widget's detail dialog and, with
//!   `EDM_DEV_TYPE=text`, types into its editor, which is how the settings UI
//!   gets exercised without a pointer and a keyboard. Separate several values
//!   with `|` to type them one after another, with the save debounce in between.
//!
//! All of them are opt-in and inert in a normal run.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use log::{info, warn};

/// Applies the development hooks requested through the environment.
pub fn install_from_env(window: &crate::window::DashboardWindow) {
    let gallery = std::env::var("EDM_DEV_GALLERY")
        .ok()
        .map(|value| value.parse::<usize>().unwrap_or(0));
    if let Some(page) = gallery {
        build_gallery(window.canvas(), page);
    }
    if let Some(spec) = std::env::var_os("EDM_DEV_DRAG") {
        simulate_drag(window.canvas(), &spec.to_string_lossy());
    }
    if let Some(spec) = std::env::var_os("EDM_DEV_PATCH") {
        simulate_patch(window.canvas(), &spec.to_string_lossy());
    }
    if let Some(kind) = std::env::var_os("EDM_DEV_DETAIL") {
        let text = std::env::var("EDM_DEV_TYPE").ok();
        simulate_detail(window, &kind.to_string_lossy(), text.as_deref());
    }
    if let Some(spec) = std::env::var_os("EDM_DEV_PLUGIN") {
        add_plugins(window.canvas(), &spec.to_string_lossy());
    }
    if let Some(path) = std::env::var_os("EDM_SCREENSHOT") {
        // The gallery includes the weather widget, whose first fetch needs a
        // network round trip before there is anything to look at.
        let delay = if gallery.is_some() {
            Duration::from_millis(6000)
        } else {
            Duration::from_millis(900)
        };
        schedule_screenshot(
            window.app_window(),
            window.canvas(),
            PathBuf::from(path),
            delay,
        );
    }
}

/// Puts one page of the widget catalogue on the canvas.
///
/// `page` selects six widgets at a time, laid out in a 3x2 grid so the whole
/// set stays inside a normal window and can be looked at one screen at a time.
pub fn build_gallery(canvas: &crate::canvas::Canvas, page: usize) {
    const PER_PAGE: usize = 6;
    const COLUMNS: i32 = 3;

    canvas.clear();
    let start = page * PER_PAGE;
    let page_widgets: Vec<_> = crate::widgets::descriptors()
        .iter()
        .skip(start)
        .take(PER_PAGE)
        .collect();

    if page_widgets.is_empty() {
        warn!("ギャラリー {page} ページ目は空です");
        return;
    }

    for (index, descriptor) in page_widgets.iter().enumerate() {
        let (width, height) = descriptor.default_size;
        let column = index as i32 % COLUMNS;
        let row = index as i32 / COLUMNS;
        let instance = crate::config::WidgetInstance::new(
            descriptor.kind,
            24 + column * 400,
            24 + row * 340,
            width,
            height,
        );
        canvas.add_instance(&instance);
    }

    info!(
        "ギャラリー {} ページ目: {:?}",
        page,
        page_widgets
            .iter()
            .map(|descriptor| descriptor.kind)
            .collect::<Vec<_>>()
    );
}

/// Renders `window` to `path` once it has been mapped, then closes it.
pub fn schedule_screenshot(
    window: &adw::ApplicationWindow,
    canvas: &crate::canvas::Canvas,
    path: PathBuf,
    delay: Duration,
) {
    let weak = window.downgrade();
    let canvas = canvas.clone();
    glib::timeout_add_local_once(delay, move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        log_layout(&canvas);
        log_catalogue();
        check_detail_views(&canvas);
        match save_window(&canvas, &window, &path) {
            Ok(()) => info!("スクリーンショットを保存しました: {}", path.display()),
            Err(e) => warn!("スクリーンショットを保存できません: {e:#}"),
        }
        window.close();
    });
}

/// Adds plugin tiles by definition id (`EDM_DEV_PLUGIN="a,b"`).
///
/// Plugins never show up in the gallery, so this is how they get looked at.
pub fn add_plugins(canvas: &crate::canvas::Canvas, spec: &str) {
    for id in spec.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        let size =
            crate::plugin::find(id).map(|loaded| (loaded.plugin.size.0, loaded.plugin.size.1));
        let added = canvas.add_kind_with(
            crate::widgets::plugin::KIND,
            serde_json::json!({ "plugin": id }),
            size,
        );
        match added {
            Some(instance) => info!("プラグイン {id} を追加しました ({instance})"),
            None => warn!("プラグイン {id} を追加できません"),
        }
    }
}

/// Switches to edit mode and drags the topmost widget, so the move/snap path
/// can be exercised without a pointer. `spec` is `"x,y,dx,dy"`.
pub fn simulate_drag(canvas: &crate::canvas::Canvas, spec: &str) {
    let numbers: Vec<f64> = spec
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    let [x, y, dx, dy] = match numbers[..] {
        [x, y, dx, dy] => [x, y, dx, dy],
        _ => {
            warn!("EDM_DEV_DRAG の形式が不正です: {spec}");
            return;
        }
    };

    canvas.set_edit_mode(true);
    {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        canvas.imp().simulate_drag(x, y, dx, dy);
    }
    info!("ドラッグをシミュレートしました: ({x}, {y}) + ({dx}, {dy})");
}

/// Builds the detail view of every widget on the canvas once, so a broken
/// detail view shows up in the log instead of only when a user clicks a tile.
pub fn check_detail_views(canvas: &crate::canvas::Canvas) {
    for instance in canvas.instances() {
        let Some(context) = canvas.widget_context(&instance.id) else {
            warn!("{} の設定コンテキストがありません", instance.kind);
            continue;
        };
        let Some(descriptor) = crate::widgets::find(&instance.kind) else {
            warn!("{} は未知のウィジェットです", instance.kind);
            continue;
        };
        match (descriptor.detail)(&context) {
            Ok(_) => info!("詳細ビュー {}: OK", instance.kind),
            Err(e) => warn!("詳細ビュー {} を作れません: {e:#}", instance.kind),
        }
    }
}

/// Applies a settings patch the way a widget's detail view would.
///
/// `spec` is `"kind:key=json"`, e.g. `world_clock:hour24=false` or
/// `note:text="hello"`.
pub fn simulate_patch(canvas: &crate::canvas::Canvas, spec: &str) {
    if spec.trim().is_empty() {
        return;
    }
    let Some((kind, assignment)) = spec.split_once(':') else {
        warn!("EDM_DEV_PATCH の形式が不正です: {spec}");
        return;
    };
    let Some((key, value)) = assignment.split_once('=') else {
        warn!("EDM_DEV_PATCH に '=' がありません: {spec}");
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(value) else {
        warn!("EDM_DEV_PATCH の値が JSON ではありません: {value}");
        return;
    };

    let Some(instance) = canvas
        .instances()
        .into_iter()
        .find(|instance| instance.kind == kind)
    else {
        warn!("{kind} がキャンバスにありません");
        return;
    };
    let Some(context) = canvas.widget_context(&instance.id) else {
        warn!("{} の設定コンテキストがありません", instance.id);
        return;
    };
    context.set(key, value);
    info!("設定パッチをシミュレートしました: {kind}.{key}");
}

/// Opens a widget's detail dialog and optionally edits its editor.
///
/// This reproduces what a user does with a pointer and a keyboard, which is the
/// only way to catch bugs in the settings UI itself. `text` may hold several
/// `|` separated values: each one is typed after the previous save has had time
/// to run, which is what exercises re-arming a debounce that already fired.
pub fn simulate_detail(window: &crate::window::DashboardWindow, kind: &str, text: Option<&str>) {
    let canvas = window.canvas();
    let Some(instance) = canvas
        .instances()
        .into_iter()
        .find(|instance| instance.kind == kind)
    else {
        warn!("{kind} がキャンバスにありません");
        return;
    };
    let Some(context) = canvas.widget_context(&instance.id) else {
        warn!("{} の設定コンテキストがありません", instance.id);
        return;
    };
    let dialog = match crate::ui::inspector::present(window.app_window(), kind, &context) {
        Ok(dialog) => dialog,
        Err(e) => {
            warn!("{kind} の詳細を開けません: {e:#}");
            return;
        }
    };
    info!("{kind} の詳細ダイアログを開きました");

    let Some(text) = text else {
        return;
    };
    let Some(root) = dialog.child() else {
        warn!("詳細ダイアログに中身がありません");
        return;
    };

    let Some(editor) = find_descendant::<gtk::TextView>(&root)
        .map(Editor::View)
        .or_else(|| find_descendant::<libadwaita::EntryRow>(&root).map(Editor::Row))
    else {
        warn!("{kind} の詳細ダイアログに入力欄がありません");
        return;
    };

    for (index, step) in text.split('|').enumerate() {
        let editor = editor.clone();
        let kind = kind.to_owned();
        let step = step.to_owned();
        glib::timeout_add_local_once(TYPE_STEP * index as u32, move || {
            info!("{kind} の入力欄に「{step}」を入力します");
            editor.type_text(&step);
        });
    }
}

/// How long to wait between two typed values: longer than any save debounce, so
/// the first save has already run when the second value arrives.
const TYPE_STEP: Duration = Duration::from_millis(900);

/// The editor found inside a detail view.
#[derive(Clone)]
enum Editor {
    View(gtk::TextView),
    Row(libadwaita::EntryRow),
}

impl Editor {
    fn type_text(&self, text: &str) {
        match self {
            Self::View(view) => view.buffer().set_text(text),
            Self::Row(row) => row.set_text(text),
        }
    }
}

/// Depth first search for the first descendant of type `T`.
fn find_descendant<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Option<T> {
    if let Some(found) = root.downcast_ref::<T>() {
        return Some(found.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_descendant::<T>(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

/// Logs the picker's sections, which is how the grouping of a growing widget
/// catalogue gets checked without opening the popover by hand.
fn log_catalogue() {
    for category in crate::widgets::Category::ALL {
        let kinds: Vec<&str> = category
            .widgets()
            .iter()
            .map(|descriptor| descriptor.name)
            .collect();
        info!(
            "ピッカー {} ({}) : {}",
            category.label(),
            kinds.len(),
            kinds.join(", ")
        );
    }
}

/// Logs the geometry of every tile at a glance.
fn log_layout(canvas: &crate::canvas::Canvas) {
    for (id, kind, allocated, requested) in canvas.debug_layout() {
        info!(
            "{kind} {}: 割当 {:?} / 要求 {:?}",
            &id[..8.min(id.len())],
            allocated,
            requested
        );
    }
}

/// Renders the dashboard itself to `path`.
///
/// The canvas is the target rather than the whole window: the tiles are what
/// has to be looked at, and the header bar only adds chrome. The renderer still
/// comes from the window, which is the thing that owns a surface.
pub fn save_window(
    canvas: &crate::canvas::Canvas,
    window: &adw::ApplicationWindow,
    path: &Path,
) -> Result<()> {
    let target: gtk::Widget = canvas.clone().upcast();
    let width = target.width();
    let height = target.height();
    if width <= 0 || height <= 0 {
        return Err(anyhow!("ウィジェットがまだ割り当てられていません"));
    }

    let paintable = gtk::WidgetPaintable::new(Some(&target));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot
        .to_node()
        .ok_or_else(|| anyhow!("描画ノードを作れません"))?;

    let dump = path.with_extension("node.txt");
    node.write_to_file(&dump)
        .map_err(|e| anyhow!("ノードを書き出せません: {e}"))?;

    let renderer = window
        .native()
        .and_then(|native| native.renderer())
        .ok_or_else(|| anyhow!("レンダラを取得できません"))?;
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));

    texture.save_to_png(path)?;
    Ok(())
}
