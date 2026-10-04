//! View renderers — the drawing half of the microkernel.
//!
//! A plugin definition names one of these by `view.kind`. Generic source-driven
//! views live in `crate::widgets::plugin`; everything here owns its own data
//! (clocks sample the time, monitors sample procfs) and ignores `[source]`.
//!
//! Adding a view means: a function in one of the submodules, one match arm in
//! [`build`], and a `kind = "…"` documented in `docs/plugins.md`.

pub mod content;
pub mod media;
pub mod system;
pub mod time;
pub mod weather;

use anyhow::Result;

use crate::plugin::Plugin;
use crate::widgets::WidgetContext;

/// Builds the body of a self-driven tile.
///
/// The caller (the plugin tile) has already applied instance overrides and
/// wraps the result in the shared chrome (header, caption). Return
/// `Err` to fall back to the error placeholder.
pub fn build(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    match &definition.view {
        crate::plugin::View::Heading { .. } => content::heading(definition),
        crate::plugin::View::Note { .. } => content::note(definition, context),
        crate::plugin::View::Launcher { .. } => content::launcher(definition, context),
        crate::plugin::View::Clock { .. } => time::clock(definition, context),
        crate::plugin::View::AnalogClock { .. } => time::analog_clock(definition, context),
        crate::plugin::View::WorldClock { .. } => time::world_clock(definition, context),
        crate::plugin::View::Calendar { .. } => time::calendar(definition, context),
        crate::plugin::View::Date { .. } => time::date(definition, context),
        crate::plugin::View::Media { .. } => media::media(definition, context),
        crate::plugin::View::Weather { .. } => weather::weather(definition, context),
        crate::plugin::View::Cpu { .. } => system::cpu(definition, context),
        crate::plugin::View::Cores { .. } => system::cores(definition, context),
        crate::plugin::View::Memory { .. } => system::memory(definition, context),
        crate::plugin::View::Disk { .. } => system::disk(definition, context),
        crate::plugin::View::DiskIo { .. } => system::diskio(definition, context),
        crate::plugin::View::Network { .. } => system::network(definition, context),
        crate::plugin::View::Load { .. } => system::load(definition, context),
        crate::plugin::View::Battery { .. } => system::battery(definition, context),
        crate::plugin::View::Processes { .. } => system::processes(definition, context),
        crate::plugin::View::System { .. } => system::system(definition, context),
        // Source-driven views are rendered by the plugin tile itself.
        other => Err(anyhow::anyhow!(
            "ビュー {:?} は自己完結型ではありません",
            other
        )),
    }
}

/// True when [`build`] handles this view (i.e. it is self-driven).
pub fn owns(view: &crate::plugin::View) -> bool {
    view.self_driven()
}

/// Convenience: parse a comma separated list, trimming blanks.
pub fn csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}
