//! Easy Dashboard Maker — a native GTK4 + libadwaita desktop dashboard.
//!
//! Entry point: set up logging, install a panic hook that leaves a trace, and
//! hand control to the `AdwApplication`.

mod anim;
mod application;
mod canvas;
mod config;
mod devtools;
mod graphics;
mod net;
mod platform;
mod plugin;
mod ui;
mod util;
mod widgets;
mod window;

use std::panic;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gtk::prelude::*;

/// When the process started, used to report the start-up time.
static START: OnceLock<Instant> = OnceLock::new();

fn main() -> glib::ExitCode {
    let _ = START.set(Instant::now());

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    install_panic_hook();

    application::build().run()
}

/// Time since the process started, if it was recorded.
pub fn startup_elapsed() -> Duration {
    START.get().map_or(Duration::ZERO, |start| start.elapsed())
}

/// Runtime panics are a bug, but when one happens we at least want the
/// location and a message on stderr before the process goes down.
fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| location.to_string())
            .unwrap_or_else(|| "不明な位置".to_owned());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "詳細不明".to_owned());
        log::error!("内部エラー ({location}): {payload}");
        default_hook(info);
    }));
}
