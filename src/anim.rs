//! Spring based animation helpers.
//!
//! libadwaita's `AdwSpringAnimation` gives us the physics based, slightly
//! overshooting motion the design targets: no fixed duration, damping decides
//! how it settles. It also honours the desktop wide "reduce animations"
//! setting via [`enabled`].

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

/// Damping, mass, stiffness. Slightly under-damped: a small, quick overshoot
/// that reads as "alive" without being bouncy.
const DAMPING: f64 = 0.85;
const MASS: f64 = 1.0;
const STIFFNESS: f64 = 240.0;

/// Respects the `gtk-enable-animations` setting (GNOME's "reduce motion" maps
/// onto it for GTK applications).
pub fn enabled() -> bool {
    gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

fn params() -> adw::SpringParams {
    adw::SpringParams::new(DAMPING, MASS, STIFFNESS)
}

/// Runs a spring from `0.0` to `1.0` and reports the progress to `on_progress`.
///
/// The returned animation keeps running as long as it is alive, so callers must
/// store it (see `Canvas::track_animation`). Progress can leave `0.0..=1.0`
/// while overshooting, which is the point.
pub fn spring(
    widget: &impl IsA<gtk::Widget>,
    on_progress: impl Fn(f64) + 'static,
) -> adw::SpringAnimation {
    let target = adw::CallbackAnimationTarget::new(on_progress);
    let animation = adw::SpringAnimation::new(widget, 0.0, 1.0, params(), target);
    animation.play();
    animation
}
