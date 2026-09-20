//! GObject implementation of [`super::Canvas`].
//!
//! The canvas is a hand written container: it measures itself to fit its
//! children, allocates them at absolute positions, and decides what a drag
//! means by hit testing its own rectangles. Everything visual that is not a
//! child widget (grid, alignment guides, resize grip) is painted in
//! `snapshot`, which keeps the widget tree small and the interaction cheap.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glib::subclass::prelude::*;
use gtk::gdk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::geometry::{Guide, Rect, SnapTargets, snap_move, snap_resize};
use crate::anim;
use crate::config::WidgetInstance;
use crate::widgets::{self, WidgetContext};

/// Grab area of the bottom right resize corner.
const GRIP_SIZE: i32 = 18;
/// Breathing room kept around the outermost widget.
const MARGIN: i32 = 24;
/// The canvas never measures smaller than this, so there is always a surface
/// to drop widgets on.
const MIN_CANVAS: (i32, i32) = (640, 480);
/// Distance at which a widget snaps onto a guide or neighbour.
const SNAP_THRESHOLD: i32 = 8;

/// One widget on the canvas.
pub struct CanvasChild {
    pub id: String,
    pub kind: String,
    pub config: RefCell<serde_json::Value>,
    /// Tile widget: the widget's own root plus the `.edm-tile` look.
    pub tile: RefCell<gtk::Widget>,
    pub rect: Cell<Rect>,
    pub min_size: Cell<(i32, i32)>,
    /// Fixed ratio (width / height) this widget type prefers, if any.
    pub aspect: Option<f64>,
    pub aspect_locked: Cell<bool>,
    /// Set by the widget itself: "there is nothing worth showing right now".
    /// Such a tile is skipped outside edit mode, but keeps its place and comes
    /// back while editing, so it can always be found and removed again.
    pub hidden: Cell<bool>,
}

impl CanvasChild {
    pub fn instance(&self) -> WidgetInstance {
        let rect = self.rect.get();
        WidgetInstance {
            id: self.id.clone(),
            kind: self.kind.clone(),
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            aspect_locked: self.aspect_locked.get(),
            config: self.config.borrow().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Move,
    Resize,
}

/// Notified whenever the layout changed.
pub(crate) type ChangeCallback = Rc<dyn Fn()>;
/// Notified when a tile is activated, with the widget id.
pub(crate) type ActivateCallback = Rc<dyn Fn(&str)>;
/// Entry point of a per widget context menu item.
type MenuAction = fn(&Canvas, &str);

#[derive(Debug, Clone)]
struct Drag {
    id: String,
    kind: DragKind,
    start: Rect,
}

#[derive(Default)]
pub struct Canvas {
    pub(crate) children: RefCell<Vec<Rc<CanvasChild>>>,
    pub(crate) selected: RefCell<Option<String>>,
    pub(crate) edit_mode: Cell<bool>,
    pub(crate) grid: Cell<i32>,
    pub(crate) snap: Cell<bool>,
    pub(crate) show_grid: Cell<bool>,
    guides: RefCell<Vec<Guide>>,
    drag: RefCell<Option<Drag>>,
    /// Set on button press, cleared by a drag: distinguishes a click from a
    /// move so that releasing a drag does not open the detail dialog.
    dragged: Cell<bool>,
    menu_target: RefCell<Option<String>>,
    animations: RefCell<Vec<adw::Animation>>,
    pub(crate) on_changed: RefCell<Option<ChangeCallback>>,
    pub(crate) on_activate: RefCell<Option<ActivateCallback>>,
}

#[glib::object_subclass]
impl ObjectSubclass for Canvas {
    const NAME: &'static str = "EdmCanvas";
    type Type = super::Canvas;
    type ParentType = gtk::Widget;
}

impl ObjectImpl for Canvas {
    fn constructed(&self) {
        self.parent_constructed();

        let canvas = self.obj();
        canvas.add_css_class("edm-canvas");
        canvas.set_overflow(gtk::Overflow::Hidden);
        canvas.set_focusable(true);
        canvas.set_can_target(true);

        self.install_gestures();
    }

    /// A container owns the widgets it was handed, so it has to release them
    /// again before being finalised.
    fn dispose(&self) {
        for child in self.children.borrow().iter() {
            child.tile.borrow().unparent();
        }
        self.children.borrow_mut().clear();
    }
}

impl Canvas {
    fn install_gestures(&self) {
        let canvas = self.obj();

        // Primary click: selection and activation.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        let weak = canvas.downgrade();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().on_press(x, y);
            }
        });
        let weak = canvas.downgrade();
        click.connect_released(move |_, _, x, y| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().on_release(x, y);
            }
        });
        canvas.add_controller(click);

        // Secondary click: per widget menu.
        let secondary = gtk::GestureClick::new();
        secondary.set_button(gtk::gdk::BUTTON_SECONDARY);
        let weak = canvas.downgrade();
        secondary.connect_pressed(move |_, _, x, y| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().show_context_menu(x, y);
            }
        });
        canvas.add_controller(secondary);

        // Drag: moving and resizing tiles in edit mode.
        let drag = gtk::GestureDrag::new();
        let weak = canvas.downgrade();
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().drag_begin(x, y);
            }
        });
        let weak = canvas.downgrade();
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().drag_update(dx, dy);
            }
        });
        let weak = canvas.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().drag_end();
            }
        });
        canvas.add_controller(drag);

        // Keyboard nudging and removal.
        let keys = gtk::EventControllerKey::new();
        let weak = canvas.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
            Some(canvas) => canvas.imp().on_key(key, state),
            None => glib::Propagation::Proceed,
        });
        canvas.add_controller(keys);
    }

    // -- children ---------------------------------------------------------

    pub(crate) fn children(&self) -> Vec<Rc<CanvasChild>> {
        self.children.borrow().clone()
    }

    pub(crate) fn child(&self, index: usize) -> Option<Rc<CanvasChild>> {
        self.children.borrow().get(index).cloned()
    }

    pub(crate) fn child_by_id(&self, id: &str) -> Option<Rc<CanvasChild>> {
        self.children.borrow().iter().find(|c| c.id == id).cloned()
    }

    /// Applies a child's hidden flag to its tile. Edit mode always reveals every
    /// tile, so a widget that hides itself stays reachable.
    fn apply_hidden(&self, child: &CanvasChild) {
        child
            .tile
            .borrow()
            .set_visible(!child.hidden.get() || self.edit_mode.get());
    }

    /// Hides or shows a tile, as asked by the widget living in it.
    fn set_hidden(&self, id: &str, hidden: bool) {
        let Some(child) = self.child_by_id(id) else {
            return;
        };
        if child.hidden.get() == hidden {
            return;
        }
        child.hidden.set(hidden);
        self.apply_hidden(&child);
    }

    fn hidden_setter(&self, id: &str) -> Rc<dyn Fn(bool)> {
        let weak = self.obj().downgrade();
        let id = id.to_owned();
        Rc::new(move |hidden| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().set_hidden(&id, hidden);
            }
        })
    }

    /// Toggles between browse mode (clicks open details) and edit mode
    /// (tiles can be moved, resized and deleted).
    pub(crate) fn set_edit_mode(&self, enabled: bool) {
        if self.edit_mode.get() == enabled {
            return;
        }
        self.edit_mode.set(enabled);

        let canvas = self.obj();
        if enabled {
            canvas.add_css_class("edit");
        } else {
            canvas.remove_css_class("edit");
            self.drag.borrow_mut().take();
            self.guides.borrow_mut().clear();
        }

        // A non targetable tile lets every pointer event reach the canvas,
        // which is what we want while editing.
        for child in self.children().iter() {
            {
                let tile = child.tile.borrow();
                tile.set_can_target(!enabled);
                if !enabled {
                    tile.remove_css_class("selected");
                }
            }
            // Leaving edit mode hides the tiles that asked to be hidden again.
            self.apply_hidden(child);
        }
        if !enabled {
            *self.selected.borrow_mut() = None;
        } else if let Some(child) = self.selected_child() {
            child.tile.borrow().add_css_class("selected");
        }
        canvas.queue_draw();
    }

    fn index_at(&self, x: f64, y: f64) -> Option<usize> {
        let (x, y) = (x.round() as i32, y.round() as i32);
        // Topmost first: later children are painted on top.
        self.children
            .borrow()
            .iter()
            .rposition(|child| child.rect.get().contains(x, y))
    }

    fn selected_child(&self) -> Option<Rc<CanvasChild>> {
        let id = self.selected.borrow().clone()?;
        self.child_by_id(&id)
    }

    /// Selects a widget, updating the selection ring on the tiles.
    fn set_selection(&self, id: Option<String>) {
        if *self.selected.borrow() == id {
            return;
        }
        *self.selected.borrow_mut() = id.clone();
        for child in self.children().iter() {
            let tile = child.tile.borrow();
            if id.as_deref() == Some(child.id.as_str()) {
                tile.add_css_class("selected");
            } else {
                tile.remove_css_class("selected");
            }
        }
        self.obj().queue_draw();
    }

    /// Moves a child to the end of the paint order and returns its new index.
    fn raise(&self, index: usize) -> usize {
        let mut children = self.children.borrow_mut();
        if index + 1 >= children.len() {
            return index;
        }
        let child = children.remove(index);
        children.push(child);
        children.len() - 1
    }

    /// Measures a freshly built tile and reconciles the result with the size
    /// the user asked for.
    fn fresh_rect(&self, tile: &gtk::Widget, wanted: Rect, kind: &str) -> (Rect, (i32, i32)) {
        let measured = (
            tile.measure(gtk::Orientation::Horizontal, -1).0,
            tile.measure(gtk::Orientation::Vertical, -1).1,
        );
        let advertised = widgets::find(kind)
            .map(|descriptor| descriptor.min_size())
            .unwrap_or((120, 72));
        let min = (measured.0.max(advertised.0), measured.1.max(advertised.1));
        (wanted.with_min_size(min.0, min.1), min)
    }

    /// Wraps widget content in the tile that gets positioned on the canvas.
    ///
    /// The wrapper is what the canvas allocates, so the content is free to
    /// centre or stretch itself inside the tile without shrinking it.
    fn wrap_tile(content: &gtk::Widget) -> gtk::Box {
        let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tile.add_css_class("edm-tile");
        tile.set_halign(gtk::Align::Fill);
        tile.set_valign(gtk::Align::Fill);
        tile.append(content);
        tile
    }

    /// Adds a widget to the canvas.
    pub(crate) fn insert(
        &self,
        instance: WidgetInstance,
        animate: bool,
        select: bool,
    ) -> Option<Rc<CanvasChild>> {
        let set_config = self.config_setter(&instance.id);
        let get_config = self.config_getter(&instance.id);
        let set_hidden = self.hidden_setter(&instance.id);
        let content = widgets::build_tile(&instance, set_config, get_config, set_hidden);
        let tile: gtk::Widget = Self::wrap_tile(&content).upcast();
        tile.set_can_target(!self.edit_mode.get());
        tile.set_parent(&*self.obj());

        let wanted = Rect::new(instance.x, instance.y, instance.w, instance.h);
        let (rect, min) = self.fresh_rect(&tile, wanted, &instance.kind);
        let descriptor = widgets::find(&instance.kind);

        let child = Rc::new(CanvasChild {
            id: instance.id.clone(),
            kind: instance.kind.clone(),
            config: RefCell::new(instance.config.clone()),
            tile: RefCell::new(tile.clone()),
            rect: Cell::new(rect),
            min_size: Cell::new(min),
            aspect: descriptor.and_then(|d| d.aspect),
            aspect_locked: Cell::new(instance.aspect_locked),
            hidden: Cell::new(false),
        });

        self.children.borrow_mut().push(child.clone());
        self.apply_hidden(&child);

        if select {
            self.set_selection(Some(child.id.clone()));
        }

        if animate && anim::enabled() {
            child.rect.set(rect.scaled_about_center(0.94));
            self.animate_rect(&child, rect, false);
            self.animate_opacity(&tile, 1.0);
        }

        self.obj().queue_resize();
        // Adding a widget changes the layout as much as moving one does: the
        // empty state has to go away and the new layout has to be saved.
        self.emit_changed();
        Some(child)
    }

    /// Replaces a tile in place, keeping position and size. Used when a widget's
    /// settings change.
    pub(crate) fn rebuild(&self, id: &str) {
        let Some(child) = self.child_by_id(id) else {
            return;
        };
        let instance = child.instance();
        let set_config = self.config_setter(&instance.id);
        let get_config = self.config_getter(&instance.id);
        let set_hidden = self.hidden_setter(&instance.id);
        let content = widgets::build_tile(&instance, set_config, get_config, set_hidden);
        let tile: gtk::Widget = Self::wrap_tile(&content).upcast();
        tile.set_can_target(!self.edit_mode.get());
        if self.selected.borrow().as_deref() == Some(id) {
            tile.add_css_class("selected");
        }

        let old: gtk::Widget = child.tile.borrow().clone();
        old.unparent();
        tile.set_parent(&*self.obj());

        let (rect, min) = self.fresh_rect(&tile, child.rect.get(), &instance.kind);
        child.rect.set(rect);
        child.min_size.set(min);
        *child.tile.borrow_mut() = tile;
        self.apply_hidden(&child);

        self.obj().queue_resize();
    }

    pub(crate) fn remove(&self, id: &str) {
        let Some(index) = self.children.borrow().iter().position(|c| c.id == id) else {
            return;
        };
        let removed = self.children.borrow_mut().remove(index);
        removed.tile.borrow().unparent();
        if self.selected.borrow().as_deref() == Some(id) {
            *self.selected.borrow_mut() = None;
        }
        self.obj().queue_resize();
        self.emit_changed();
    }

    /// Applies a settings patch from a widget, merging it into the live value so
    /// an open detail view can never write stale keys back.
    fn patch_config(&self, id: &str, patch: serde_json::Value) {
        let Some(child) = self.child_by_id(id) else {
            return;
        };
        let current = child.config.borrow().clone();
        let merged = merge_config(current.clone(), patch);
        if merged == current {
            // Nothing actually changed (a text row confirming what it already
            // saved, say): rebuilding would only make the tile flicker.
            return;
        }
        *child.config.borrow_mut() = merged;
        self.rebuild(id);
        self.emit_changed();
    }

    fn config_setter(&self, id: &str) -> Rc<dyn Fn(serde_json::Value)> {
        let weak = self.obj().downgrade();
        let id = id.to_owned();
        Rc::new(move |value| {
            if let Some(canvas) = weak.upgrade() {
                canvas.imp().patch_config(&id, value);
            }
        })
    }

    /// Reads an instance's live settings. See `WidgetContext::current`.
    fn config_getter(&self, id: &str) -> Rc<dyn Fn() -> serde_json::Value> {
        let weak = self.obj().downgrade();
        let id = id.to_owned();
        Rc::new(move || {
            weak.upgrade()
                .and_then(|canvas| canvas.imp().child_config(&id))
                .unwrap_or(serde_json::Value::Null)
        })
    }

    fn child_config(&self, id: &str) -> Option<serde_json::Value> {
        Some(self.child_by_id(id)?.config.borrow().clone())
    }

    /// Context handed to a widget so it can read and persist its own settings.
    pub(crate) fn widget_context(&self, id: &str) -> Option<WidgetContext> {
        let child = self.child_by_id(id)?;
        Some(WidgetContext::new(
            child.id.clone(),
            child.kind.clone(),
            child.config.borrow().clone(),
            self.config_setter(id),
            self.config_getter(id),
            self.hidden_setter(id),
        ))
    }

    /// Where a new widget of `size` should go.
    pub(crate) fn free_spot(&self, size: (i32, i32)) -> (i32, i32) {
        let occupied: Vec<Rect> = self.children().iter().map(|c| c.rect.get()).collect();
        let canvas = self.obj();
        let area = (
            canvas.width().max(MIN_CANVAS.0),
            canvas.height().max(MIN_CANVAS.1),
        );
        super::geometry::free_spot(size, &occupied, area, self.grid.get().max(8))
    }

    // -- interaction ------------------------------------------------------

    fn on_press(&self, x: f64, y: f64) {
        self.dragged.set(false);
        self.obj().grab_focus();

        if !self.edit_mode.get() {
            return;
        }
        match self.index_at(x, y) {
            Some(index) => {
                let id = self.child(index).map(|child| child.id.clone());
                self.set_selection(id);
            }
            None => self.set_selection(None),
        }
    }

    fn on_release(&self, x: f64, y: f64) {
        if self.edit_mode.get() || self.dragged.get() {
            return;
        }
        let Some(index) = self.index_at(x, y) else {
            return;
        };
        let Some(child) = self.child(index) else {
            return;
        };
        self.emit_activate(&child.id);
    }

    /// Runs one move gesture the way a pointer drag would. Only used by the
    /// development tools, which cannot deliver real input events.
    pub(crate) fn simulate_drag(&self, x: f64, y: f64, dx: f64, dy: f64) {
        self.drag_begin(x, y);
        self.drag_update(dx, dy);
        self.drag_end();
    }

    fn drag_begin(&self, x: f64, y: f64) {
        self.dragged.set(true);
        if !self.edit_mode.get() {
            return;
        }

        let Some(index) = self.index_at(x, y) else {
            return;
        };
        let Some(child) = self.child(index) else {
            return;
        };
        let rect = child.rect.get();
        let selected = self.selected.borrow().as_deref() == Some(child.id.as_str());

        // The grip only answers on the selected tile, so a first drag never
        // resizes by accident.
        let kind = if selected && rect.contains_grip(x.round() as i32, y.round() as i32, GRIP_SIZE)
        {
            DragKind::Resize
        } else {
            DragKind::Move
        };

        let index = self.raise(index);
        debug_assert!(index < self.children.borrow().len());
        self.set_selection(Some(child.id.clone()));
        *self.drag.borrow_mut() = Some(Drag {
            id: child.id.clone(),
            kind,
            start: rect,
        });
        self.obj().queue_draw();
    }

    fn drag_update(&self, dx: f64, dy: f64) {
        let Some(drag) = self.drag.borrow().clone() else {
            return;
        };
        let Some(child) = self.child_by_id(&drag.id) else {
            return;
        };

        let raw = match drag.kind {
            DragKind::Move => Rect::new(
                drag.start.x + dx.round() as i32,
                drag.start.y + dy.round() as i32,
                drag.start.w,
                drag.start.h,
            )
            .at_least_origin(),
            DragKind::Resize => self.resize_rect(&child, drag.start, dx, dy),
        };

        // While dragging the widget follows the pointer freely; the guides only
        // preview where it would land, and `drag_end` springs it there.
        let snap = self.snap(raw, &child, drag.kind);
        *self.guides.borrow_mut() = snap.guides;
        child.rect.set(raw);
        self.obj().queue_resize();
    }

    fn resize_rect(&self, child: &CanvasChild, start: Rect, dx: f64, dy: f64) -> Rect {
        let min = child.min_size.get();
        let mut rect = Rect::new(
            start.x,
            start.y,
            start.w + dx.round() as i32,
            start.h + dy.round() as i32,
        )
        .with_min_size(min.0, min.1);

        if child.aspect_locked.get() {
            if let Some(aspect) = child.aspect.filter(|a| *a > 0.0) {
                rect.h = ((rect.w as f64) / aspect).round().max(f64::from(min.1)) as i32;
            }
        }
        rect
    }

    fn drag_end(&self) {
        let Some(drag) = self.drag.borrow_mut().take() else {
            return;
        };
        let Some(child) = self.child_by_id(&drag.id) else {
            return;
        };
        let current = child.rect.get();
        let target = self.snap(current, &child, drag.kind).rect;
        if target != current {
            self.animate_rect(&child, target, true);
        } else {
            self.guides.borrow_mut().clear();
            self.obj().queue_draw();
        }
        self.emit_changed();
    }

    fn snap_targets(&self, exclude: &str) -> SnapTargets {
        SnapTargets {
            grid: self.grid.get(),
            threshold: SNAP_THRESHOLD,
            bounds: (
                self.obj().width().max(MIN_CANVAS.0),
                self.obj().height().max(MIN_CANVAS.1),
            ),
            others: self
                .children()
                .iter()
                .filter(|c| c.id != exclude)
                .map(|c| c.rect.get())
                .collect(),
        }
    }

    fn snap(&self, raw: Rect, child: &CanvasChild, kind: DragKind) -> super::geometry::Snap {
        if !self.snap.get() {
            return super::geometry::Snap {
                rect: raw,
                guides: Vec::new(),
            };
        }
        let targets = self.snap_targets(&child.id);
        match kind {
            DragKind::Resize => {
                let aspect = if child.aspect_locked.get() {
                    child.aspect
                } else {
                    None
                };
                snap_resize(raw, &targets, aspect, child.min_size.get())
            }
            DragKind::Move => snap_move(raw, &targets),
        }
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if !self.edit_mode.get() {
            return glib::Propagation::Proceed;
        }

        if key == gdk::Key::Escape {
            self.set_selection(None);
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::Delete {
            if let Some(child) = self.selected_child() {
                self.remove(&child.id);
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }

        let step = if state.contains(gdk::ModifierType::SHIFT_MASK) {
            self.grid.get().max(1)
        } else {
            1
        };
        let (dx, dy) = match key {
            gdk::Key::Left => (-step, 0),
            gdk::Key::Right => (step, 0),
            gdk::Key::Up => (0, -step),
            gdk::Key::Down => (0, step),
            _ => return glib::Propagation::Proceed,
        };

        let Some(child) = self.selected_child() else {
            return glib::Propagation::Proceed;
        };
        let rect = child.rect.get();
        child
            .rect
            .set(Rect::new(rect.x + dx, rect.y + dy, rect.w, rect.h).at_least_origin());
        self.obj().queue_resize();
        self.emit_changed();
        glib::Propagation::Stop
    }

    fn show_context_menu(&self, x: f64, y: f64) {
        let Some(index) = self.index_at(x, y) else {
            return;
        };
        let Some(child) = self.child(index) else {
            return;
        };
        self.set_selection(Some(child.id.clone()));
        *self.menu_target.borrow_mut() = Some(child.id.clone());

        let popover = gtk::Popover::new();
        popover.add_css_class("menu");
        popover.set_has_arrow(true);
        popover.set_parent(&*self.obj());
        popover.set_pointing_to(Some(&gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            1,
            1,
        )));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(6);
        content.set_margin_end(6);

        let locked = child.aspect_locked.get();
        let entries: [(&str, &str, MenuAction); 3] = [
            (
                "詳細を表示",
                "dialog-information-symbolic",
                |canvas, id| {
                    canvas.emit_activate(id);
                },
            ),
            (
                if locked {
                    "縦横比の固定を解除"
                } else {
                    "縦横比を固定"
                },
                "changes-prevent-symbolic",
                |canvas, id| canvas.toggle_aspect_lock(id),
            ),
            ("削除", "user-trash-symbolic", |canvas, id| {
                canvas.remove(id);
            }),
        ];

        for (label, icon, action) in entries {
            let weak = self.obj().downgrade();
            let popover_weak = popover.downgrade();
            let button = menu_button(label, icon, move || {
                let Some(canvas) = weak.upgrade() else {
                    return;
                };
                let target = canvas.imp().menu_target.borrow().clone();
                if let Some(id) = target {
                    action(canvas.imp(), &id);
                }
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
            });
            content.append(&button);
        }

        popover.set_child(Some(&content));
        popover.connect_closed(|popover| {
            popover.unparent();
        });
        popover.popup();
    }

    fn toggle_aspect_lock(&self, id: &str) {
        let Some(child) = self.child_by_id(id) else {
            return;
        };
        let locked = !child.aspect_locked.get();
        child.aspect_locked.set(locked);

        if locked {
            if let Some(aspect) = child.aspect.filter(|a| *a > 0.0) {
                let rect = child.rect.get();
                let min = child.min_size.get();
                let target = Rect::new(
                    rect.x,
                    rect.y,
                    rect.w,
                    (rect.w as f64 / aspect).round() as i32,
                )
                .with_min_size(min.0, min.1);
                self.animate_rect(&child, target, false);
            }
        }
        self.emit_changed();
    }

    // -- animation --------------------------------------------------------

    fn track_animation(&self, animation: adw::SpringAnimation) {
        let mut animations = self.animations.borrow_mut();
        animations.retain(|a| a.state() != adw::AnimationState::Finished);
        animations.push(animation.upcast());
    }

    fn animate_rect(&self, child: &Rc<CanvasChild>, target: Rect, settle: bool) {
        let from = child.rect.get();
        if from == target || !anim::enabled() {
            child.rect.set(target);
            self.guides.borrow_mut().clear();
            self.obj().queue_resize();
            return;
        }

        let weak = self.obj().downgrade();
        let animated = child.clone();
        let animation = anim::spring(&*self.obj(), move |t| {
            animated.rect.set(from.lerp(target, t));
            if let Some(canvas) = weak.upgrade() {
                canvas.queue_resize();
            }
        });

        if settle {
            let weak = self.obj().downgrade();
            animation.connect_done(move |_| {
                if let Some(canvas) = weak.upgrade() {
                    canvas.imp().guides.borrow_mut().clear();
                    canvas.queue_draw();
                }
            });
        }
        self.track_animation(animation);
    }

    fn animate_opacity(&self, widget: &gtk::Widget, target: f64) {
        if !anim::enabled() {
            widget.set_opacity(target);
            return;
        }
        let weak = widget.downgrade();
        let animation = anim::spring(widget, move |t| {
            if let Some(widget) = weak.upgrade() {
                widget.set_opacity((t * target).clamp(0.0, 1.0));
            }
        });
        self.track_animation(animation);
    }

    // -- callbacks --------------------------------------------------------

    pub(crate) fn emit_changed(&self) {
        let callback = self.on_changed.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    fn emit_activate(&self, id: &str) {
        let callback = self.on_activate.borrow().clone();
        if let Some(callback) = callback {
            callback(id);
        }
    }

    // -- painting ---------------------------------------------------------

    fn content_extent(&self) -> (i32, i32) {
        let mut width = MIN_CANVAS.0;
        let mut height = MIN_CANVAS.1;
        for child in self.children().iter() {
            let rect = child.rect.get();
            width = width.max(rect.right() + MARGIN);
            height = height.max(rect.bottom() + MARGIN);
        }
        (width, height)
    }

    fn dark(&self) -> bool {
        adw::StyleManager::default().is_dark()
    }

    fn guide_color(&self) -> gtk::gdk::RGBA {
        if self.dark() {
            gtk::gdk::RGBA::new(0.47, 0.68, 0.93, 0.9)
        } else {
            gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 0.9)
        }
    }

    fn paint_grid(&self, snapshot: &gtk::Snapshot, width: i32, height: i32) {
        let step = self.grid.get().max(4);
        let color = if self.dark() {
            gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 0.045)
        } else {
            gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.055)
        };

        let mut x = step;
        while x < width {
            snapshot.append_color(
                &color,
                &gtk::graphene::Rect::new(x as f32, 0.0, 1.0, height as f32),
            );
            x += step;
        }
        let mut y = step;
        while y < height {
            snapshot.append_color(
                &color,
                &gtk::graphene::Rect::new(0.0, y as f32, width as f32, 1.0),
            );
            y += step;
        }
    }

    fn paint_guides(&self, snapshot: &gtk::Snapshot, width: i32, height: i32) {
        let color = self.guide_color();
        for guide in self.guides.borrow().iter() {
            let rect = match *guide {
                Guide::Vertical(x) => gtk::graphene::Rect::new(x as f32, 0.0, 1.0, height as f32),
                Guide::Horizontal(y) => gtk::graphene::Rect::new(0.0, y as f32, width as f32, 1.0),
            };
            snapshot.append_color(&color, &rect);
        }
    }

    fn paint_grip(&self, snapshot: &gtk::Snapshot, rect: Rect) {
        let size = GRIP_SIZE as f32;
        let inset = 5.0;
        let bounds = gtk::graphene::Rect::new(
            rect.right() as f32 - size - inset,
            rect.bottom() as f32 - size - inset,
            size,
            size,
        );
        let radius = gtk::graphene::Size::new(size / 2.0, size / 2.0);
        let rounded = gtk::gsk::RoundedRect::new(bounds, radius, radius, radius, radius);
        let color = self.guide_color();
        snapshot.append_border(&rounded, &[2.0; 4], &[color; 4]);
    }
}

impl WidgetImpl for Canvas {
    fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
        let (width, height) = self.content_extent();
        let size = match orientation {
            gtk::Orientation::Horizontal => width,
            _ => height,
        };
        (size, size, -1, -1)
    }

    fn size_allocate(&self, _width: i32, _height: i32, _baseline: i32) {
        for child in self.children().iter() {
            let rect = child.rect.get();
            let matrix = gtk::graphene::Matrix::new_translate(&gtk::graphene::Point3D::new(
                rect.x as f32,
                rect.y as f32,
                0.0,
            ));
            let transform = gtk::gsk::Transform::new().matrix(&matrix);
            child
                .tile
                .borrow()
                .allocate(rect.w, rect.h, -1, Some(transform));
        }
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let canvas = self.obj();
        let (width, height) = (canvas.width(), canvas.height());

        if self.show_grid.get() && self.edit_mode.get() {
            self.paint_grid(snapshot, width, height);
        }

        for child in self.children().iter() {
            canvas.snapshot_child(&*child.tile.borrow(), snapshot);
        }

        if self.edit_mode.get() {
            self.paint_guides(snapshot, width, height);
            if let Some(child) = self.selected_child() {
                self.paint_grip(snapshot, child.rect.get());
            }
        }
    }
}

/// Merges a settings patch: objects are merged key by key, anything else
/// replaces the stored value.
fn merge_config(current: serde_json::Value, patch: serde_json::Value) -> serde_json::Value {
    match (current, patch) {
        (serde_json::Value::Object(mut current), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                current.insert(key, value);
            }
            serde_json::Value::Object(current)
        }
        (_, patch) => patch,
    }
}

/// A flat button with an icon and a left aligned label, used by the per widget
/// context menu.
fn menu_button(label: &str, icon: &str, activate: impl Fn() + 'static) -> gtk::Button {
    let image = gtk::Image::from_icon_name(icon);
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.append(&image);
    content.append(&text);

    let button = gtk::Button::builder()
        .has_frame(false)
        .child(&content)
        .build();
    button.add_css_class("flat");
    button.connect_clicked(move |_| activate());
    button
}
