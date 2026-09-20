//! The dashboard canvas.
//!
//! A container that hosts freely placed, resizable widget tiles on a large
//! surface, with snapping, keyboard nudging and per widget settings. It knows
//! nothing about specific widgets: it builds them from the [`crate::widgets`]
//! registry and hands them a context so they can persist their own settings.

mod geometry;
mod imp;

use std::rc::Rc;

use glib::subclass::prelude::ObjectSubclassIsExt;
use gtk::prelude::*;

use crate::config::WidgetInstance;
use crate::widgets::WidgetContext;

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Canvas {
    /// Creates an empty canvas in browse mode.
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// In edit mode tiles can be dragged, resized and deleted; otherwise a
    /// click opens the widget's detail view.
    pub fn set_edit_mode(&self, enabled: bool) {
        self.imp().set_edit_mode(enabled);
    }

    pub fn is_edit_mode(&self) -> bool {
        self.imp().edit_mode.get()
    }

    /// Grid step, in logical pixels.
    pub fn set_grid(&self, grid: i32) {
        self.imp().grid.set(grid.max(2));
        self.queue_draw();
    }

    pub fn set_snap_enabled(&self, enabled: bool) {
        self.imp().snap.set(enabled);
    }

    pub fn set_show_grid(&self, enabled: bool) {
        self.imp().show_grid.set(enabled);
        self.queue_draw();
    }

    /// Adds a widget from a saved or freshly built instance.
    pub fn add_instance(&self, instance: &WidgetInstance) {
        self.imp().insert(instance.clone(), false, false);
    }

    /// Adds a new widget of `kind` at the first free spot and selects it.
    pub fn add_kind(&self, kind: &str) -> Option<String> {
        self.add_kind_with(kind, serde_json::Value::Null, None)
    }

    /// Same, with settings the widget starts from and, for widgets whose size
    /// comes from somewhere else (a plugin definition), an explicit size.
    pub fn add_kind_with(
        &self,
        kind: &str,
        config: serde_json::Value,
        size: Option<(i32, i32)>,
    ) -> Option<String> {
        let (w, h) = size.or_else(|| crate::widgets::find(kind).map(|d| d.default_size))?;
        let (x, y) = self.imp().free_spot((w, h));
        let mut instance = WidgetInstance::new(kind, x, y, w, h);
        instance.config = config;
        let child = self.imp().insert(instance, true, true)?;
        self.imp().emit_changed();
        Some(child.id.clone())
    }

    /// Rebuilds every tile of `kind`.
    ///
    /// Used when the thing the tiles were built from changed outside the app,
    /// e.g. after a plugin definition was edited.
    pub fn refresh_kind(&self, kind: &str) {
        let ids: Vec<String> = self
            .imp()
            .children()
            .iter()
            .filter(|child| child.kind == kind)
            .map(|child| child.id.clone())
            .collect();
        for id in ids {
            self.imp().rebuild(&id);
        }
    }

    pub fn remove_selected(&self) {
        let id = self.imp().selected.borrow().clone();
        if let Some(id) = id {
            self.imp().remove(&id);
        }
    }

    /// Removes every widget, e.g. when resetting the layout.
    pub fn clear(&self) {
        for id in self
            .imp()
            .children()
            .iter()
            .map(|child| child.id.clone())
            .collect::<Vec<_>>()
        {
            self.imp().remove(&id);
        }
    }

    /// Current layout, ready to be persisted.
    pub fn instances(&self) -> Vec<WidgetInstance> {
        self.imp()
            .children()
            .iter()
            .map(|child| child.instance())
            .collect()
    }

    pub fn widget_count(&self) -> usize {
        self.imp().children.borrow().len()
    }

    /// `(id, kind, allocated size, requested rect)` for every tile, for
    /// development checks.
    pub fn debug_layout(&self) -> Vec<(String, String, [i32; 2], [i32; 4])> {
        self.imp()
            .children()
            .iter()
            .map(|child| {
                let rect = child.rect.get();
                let tile = child.tile.borrow();
                (
                    child.id.clone(),
                    child.kind.clone(),
                    [tile.width(), tile.height()],
                    [rect.x, rect.y, rect.w, rect.h],
                )
            })
            .collect()
    }

    /// Settings handle for the widget with `id`, used by the detail dialog.
    pub fn widget_context(&self, id: &str) -> Option<WidgetContext> {
        self.imp().widget_context(id)
    }

    pub fn widget_kind(&self, id: &str) -> Option<String> {
        self.imp().child_by_id(id).map(|child| child.kind.clone())
    }

    /// Called whenever the layout changed and should be persisted.
    pub fn connect_changed(&self, callback: impl Fn() + 'static) {
        *self.imp().on_changed.borrow_mut() = Some(Rc::new(callback));
    }

    /// Called when a tile is activated (clicked) with its widget id.
    pub fn connect_activate(&self, callback: impl Fn(&str) + 'static) {
        *self.imp().on_activate.borrow_mut() = Some(Rc::new(callback));
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}
