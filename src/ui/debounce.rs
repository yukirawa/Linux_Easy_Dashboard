//! Coalescing timer for work that should not run on every keystroke.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// Runs an action once `delay` has passed without another call.
///
/// The pending source is forgotten from *inside* the callback, before the
/// action runs. That invariant matters: `glib::SourceId::remove` treats a
/// source that already fired as an error and panics, so a debounce that keeps
/// the id around after firing takes the whole application down on the next
/// keystroke — which is exactly what used to happen in the note widget.
#[derive(Clone)]
pub struct Debounce {
    pending: Rc<Cell<Option<glib::SourceId>>>,
    delay: Duration,
}

impl Debounce {
    pub fn new(delay: Duration) -> Self {
        Self {
            pending: Rc::new(Cell::new(None)),
            delay,
        }
    }

    /// Schedules `action`, replacing whatever was scheduled before.
    pub fn schedule(&self, action: impl FnOnce() + 'static) {
        self.cancel();
        let pending = Rc::clone(&self.pending);
        let source = glib::timeout_add_local_once(self.delay, move || {
            pending.set(None);
            action();
        });
        self.pending.set(Some(source));
    }

    /// Drops the scheduled action, if there is one.
    ///
    /// Safe to call at any time, including from inside the action itself.
    pub fn cancel(&self) {
        if let Some(source) = self.pending.take() {
            source.remove();
        }
    }
}
