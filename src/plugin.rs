//! User defined widgets.
//!
//! Every `*.toml` file in `$XDG_CONFIG_HOME/linux-easy-dashboard/plugins`
//! describes one widget the user wrote themselves: where the value comes from
//! ([`Source`]) and how it should be drawn ([`View`]). This module loads,
//! validates, samples and saves those files; the tile that draws one lives in
//! `crate::widgets::plugin` and identifies its definition with
//! `{"plugin": "<id>"}`.
//!
//! Nothing here touches GTK or glib: it is plain filesystem and process work, so
//! [`sample`] can be called from a worker thread while the UI stays responsive.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::config::APP_DIR;

/// Directory below `$XDG_CONFIG_HOME` holding one file per plugin.
const PLUGIN_DIR: &str = "plugins";
const EXTENSION: &str = "toml";

const MAX_ID_LEN: usize = 64;
const MIN_SIDE: i32 = 120;
const MAX_SIDE: i32 = 1200;
const MIN_REFRESH: u64 = 1;
const MAX_REFRESH: u64 = 86_400;
const MIN_TIMEOUT: u64 = 1;
const MAX_TIMEOUT: u64 = 600;
const MAX_DECIMALS: usize = 6;
const MIN_HISTORY: usize = 2;
const MAX_HISTORY: usize = 600;
const MIN_ROWS: usize = 1;
const MAX_ROWS: usize = 64;
/// Command output is drawn on a tile, so there is no point keeping more.
const MAX_OUTPUT: usize = 4096;
/// Files are read up to this many bytes.
const FILE_LIMIT: u64 = 64 * 1024;
/// How often a command is polled while waiting for it to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Where a value comes from.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// Runs through `/bin/sh` and reads standard output.
    Command {
        run: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
    /// Reads a file, at most [`FILE_LIMIT`] bytes of it.
    File { path: String },
    /// Formats the current time with a strftime pattern (and optional IANA zone).
    Clock {
        format: String,
        #[serde(default)]
        timezone: String,
    },
    /// GETs a URL and returns the body (optionally a JSON pointer).
    Http {
        url: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
        /// RFC 6901 JSON pointer, e.g. `/current/temperature_2m`.
        #[serde(default)]
        json_pointer: String,
    },
}

impl Source {
    fn normalise(&mut self) -> Result<(), String> {
        match self {
            Self::Command { run, timeout_secs } => {
                if run.trim().is_empty() {
                    return Err("source.run を指定してください".to_owned());
                }
                *timeout_secs = (*timeout_secs).clamp(MIN_TIMEOUT, MAX_TIMEOUT);
            }
            Self::File { path } => {
                if path.trim().is_empty() {
                    return Err("source.path を指定してください".to_owned());
                }
            }
            Self::Clock { format, timezone } => {
                if format.trim().is_empty() {
                    return Err("source.format を指定してください".to_owned());
                }
                *timezone = timezone.trim().to_owned();
            }
            Self::Http {
                url,
                timeout_secs,
                json_pointer,
            } => {
                if url.trim().is_empty() {
                    return Err("source.url を指定してください".to_owned());
                }
                *timeout_secs = (*timeout_secs).clamp(MIN_TIMEOUT, MAX_TIMEOUT);
                *json_pointer = json_pointer.trim().to_owned();
            }
        }
        Ok(())
    }
}

/// How the value is drawn; the UI branches on this.
///
/// The numeric views ([`View::Readout`], [`View::Bar`], [`View::Ring`] and
/// [`View::Sparkline`]) all carry the same number settings inline, so a hand
/// written file can put `scale`, `min`, `unit` … straight under `[view]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum View {
    /// A big number or short text with a caption.
    Readout {
        #[serde(default)]
        label: String,
        #[serde(default)]
        unit: String,
        #[serde(flatten)]
        number: Number,
    },
    /// A horizontal bar, filled by `min..=max`.
    Bar {
        #[serde(default)]
        unit: String,
        #[serde(flatten)]
        number: Number,
    },
    /// A ring gauge.
    Ring {
        #[serde(default)]
        unit: String,
        #[serde(flatten)]
        number: Number,
    },
    /// A line over the recent history.
    Sparkline {
        #[serde(default = "default_history")]
        history: usize,
        #[serde(default)]
        unit: String,
        #[serde(flatten)]
        number: Number,
    },
    /// A list of rows, split into a name and a value.
    List {
        #[serde(default = "default_rows")]
        rows: usize,
        #[serde(default = "default_split")]
        split: String,
        #[serde(default)]
        label_column: usize,
        #[serde(default = "default_value_column")]
        value_column: usize,
    },
    /// Read-only key/value rows, e.g. a machine summary.
    Facts {
        #[serde(default = "default_rows")]
        rows: usize,
        #[serde(default = "default_split")]
        split: String,
        #[serde(default)]
        label_column: usize,
        #[serde(default = "default_value_column")]
        value_column: usize,
    },
    /// One labelled horizontal bar per row.
    Bars {
        #[serde(default = "default_rows")]
        rows: usize,
        #[serde(default = "default_split")]
        split: String,
        #[serde(default)]
        label_column: usize,
        #[serde(default = "default_value_column")]
        value_column: usize,
        /// Only keep rows whose label contains this, when set.
        #[serde(default)]
        filter: String,
        /// `"value"` (largest first) or `"label"`; empty keeps the source order.
        #[serde(default)]
        sort: String,
        #[serde(default)]
        unit: String,
        /// Shown when there is nothing to list, e.g. no sensor found.
        #[serde(default)]
        empty: String,
        #[serde(flatten)]
        number: Number,
    },
    /// The raw text, as it came out of the source.
    Text {
        #[serde(default = "default_true")]
        wrap: bool,
    },
    // ---- settings-only / time-driven ----
    /// Section title with an accent bar.
    Heading {
        #[serde(default)]
        title: String,
        #[serde(default)]
        subtitle: String,
        #[serde(default = "default_true")]
        accent: bool,
    },
    /// Sticky note; the body is per-tile instance state.
    Note {
        #[serde(default)]
        title: String,
        #[serde(default)]
        text: String,
    },
    /// Digital clock.
    Clock {
        #[serde(default = "default_true")]
        hour24: bool,
        #[serde(default)]
        show_seconds: bool,
        #[serde(default = "default_true")]
        show_date: bool,
        /// IANA zone; empty means the system zone.
        #[serde(default)]
        timezone: String,
    },
    /// Cairo clock face.
    AnalogClock {
        #[serde(default = "default_true")]
        show_seconds: bool,
        #[serde(default)]
        show_numerals: bool,
        #[serde(default)]
        smooth_seconds: bool,
    },
    /// Several time zones side by side.
    WorldClock {
        /// Comma separated IANA identifiers.
        #[serde(default)]
        zones: String,
        #[serde(default = "default_true")]
        hour24: bool,
    },
    /// Month grid.
    Calendar {
        #[serde(default = "default_true")]
        week_starts_monday: bool,
    },
    /// Big date plus year progress.
    Date {
        #[serde(default = "default_true")]
        show_week: bool,
        #[serde(default = "default_true")]
        show_progress: bool,
    },
    // ---- interactive ----
    /// Desktop application buttons.
    Launcher {
        /// Comma separated desktop-entry ids.
        #[serde(default)]
        apps: String,
        #[serde(default = "default_columns")]
        columns: usize,
        #[serde(default = "default_true")]
        show_labels: bool,
    },
    /// MPRIS now-playing tile.
    Media {
        /// Preferred player bus suffix; empty picks the first playing one.
        #[serde(default)]
        player: String,
        #[serde(default = "default_true")]
        only_while_playing: bool,
        #[serde(default = "default_true")]
        show_controls: bool,
        #[serde(default = "default_true")]
        show_art: bool,
    },
    /// Open-Meteo current conditions and a short forecast.
    Weather {
        /// Place name; coordinates are cached once geocoded.
        #[serde(default)]
        location: String,
        #[serde(default)]
        fahrenheit: bool,
        #[serde(default = "default_days")]
        days: usize,
    },
    // ---- system monitors (kernel samples procfs/sysfs themselves) ----
    /// Per-core columns plus total history.
    Cpu {
        #[serde(default = "default_true")]
        per_core: bool,
        #[serde(default = "default_true")]
        show_history: bool,
    },
    /// Heatmap of every core.
    Cores {
        #[serde(default)]
        columns: usize,
        #[serde(default = "default_true")]
        warn_by_usage: bool,
    },
    /// RAM ring gauge and swap line.
    Memory {
        #[serde(default = "default_true")]
        show_swap: bool,
        #[serde(default = "default_true")]
        warn_by_usage: bool,
    },
    /// One ring per mount.
    Disk {
        /// Comma separated mount points; empty tracks the root filesystem.
        #[serde(default)]
        mounts: String,
        #[serde(default = "default_rows")]
        max_rows: usize,
        #[serde(default = "default_true")]
        show_free: bool,
    },
    /// Read/write rates and history.
    #[serde(rename = "diskio")]
    DiskIo {
        /// Device name; empty tracks the busiest one.
        #[serde(default)]
        device: String,
        #[serde(default = "default_true")]
        show_history: bool,
    },
    /// Receive/transmit rates and history.
    Network {
        /// Interface name; empty tracks the default route.
        #[serde(default)]
        interface: String,
        #[serde(default = "default_true")]
        show_history: bool,
    },
    /// Load average, normalised by core count.
    Load {
        #[serde(default = "default_true")]
        show_history: bool,
    },
    /// Charge ring and remaining time.
    Battery {
        #[serde(default = "default_true")]
        show_time: bool,
        #[serde(default = "default_true")]
        warn_by_charge: bool,
    },
    /// Heaviest processes.
    Processes {
        #[serde(default = "default_rows")]
        count: usize,
        #[serde(default = "default_true")]
        by_cpu: bool,
    },
    /// Combined CPU / memory / load / disk summary.
    System {
        #[serde(default = "default_true")]
        show_cpu: bool,
        #[serde(default = "default_true")]
        show_memory: bool,
        #[serde(default = "default_true")]
        show_load: bool,
        #[serde(default = "default_true")]
        show_disk: bool,
        #[serde(default = "default_disk_path")]
        disk_path: String,
    },
}

impl View {
    /// The number settings of a numeric view, if this is one.
    pub fn number(&self) -> Option<Number> {
        match self {
            Self::Readout { number, .. }
            | Self::Bar { number, .. }
            | Self::Ring { number, .. }
            | Self::Sparkline { number, .. }
            | Self::Bars { number, .. } => Some(*number),
            _ => None,
        }
    }

    /// The unit shown after the number, empty when there is none.
    pub fn unit(&self) -> &str {
        match self {
            Self::Readout { unit, .. }
            | Self::Bar { unit, .. }
            | Self::Ring { unit, .. }
            | Self::Sparkline { unit, .. }
            | Self::Bars { unit, .. } => unit,
            _ => "",
        }
    }

    /// True when the renderer owns its own data (clock, monitors, …) and the
    /// optional `[source]` is not what feeds the tile.
    pub fn self_driven(&self) -> bool {
        matches!(
            self,
            Self::Heading { .. }
                | Self::Note { .. }
                | Self::Clock { .. }
                | Self::AnalogClock { .. }
                | Self::WorldClock { .. }
                | Self::Calendar { .. }
                | Self::Date { .. }
                | Self::Launcher { .. }
                | Self::Media { .. }
                | Self::Weather { .. }
                | Self::Cpu { .. }
                | Self::Cores { .. }
                | Self::Memory { .. }
                | Self::Disk { .. }
                | Self::DiskIo { .. }
                | Self::Network { .. }
                | Self::Load { .. }
                | Self::Battery { .. }
                | Self::Processes { .. }
                | Self::System { .. }
        )
    }

    fn normalise(&mut self) {
        match self {
            Self::Readout { number, .. } | Self::Bar { number, .. } | Self::Ring { number, .. } => {
                number.normalise();
            }
            Self::Sparkline {
                history, number, ..
            } => {
                *history = (*history).clamp(MIN_HISTORY, MAX_HISTORY);
                number.normalise();
            }
            Self::List { rows, split, .. } | Self::Facts { rows, split, .. } => {
                *rows = (*rows).clamp(MIN_ROWS, MAX_ROWS);
                if split.trim().is_empty() {
                    *split = default_split();
                }
            }
            Self::Bars {
                rows,
                split,
                number,
                ..
            } => {
                *rows = (*rows).clamp(MIN_ROWS, MAX_ROWS);
                if split.trim().is_empty() {
                    *split = default_split();
                }
                number.normalise();
            }
            Self::Text { .. }
            | Self::Heading { .. }
            | Self::Note { .. }
            | Self::Clock { .. }
            | Self::AnalogClock { .. }
            | Self::Calendar { .. }
            | Self::Date { .. }
            | Self::Media { .. }
            | Self::Cpu { .. }
            | Self::Memory { .. }
            | Self::Load { .. }
            | Self::Battery { .. }
            | Self::System { .. } => {}
            Self::WorldClock { zones, .. } => {
                *zones = zones.trim().to_owned();
            }
            Self::Launcher { apps, columns, .. } => {
                *apps = apps.trim().to_owned();
                *columns = (*columns).clamp(1, MAX_ROWS);
            }
            Self::Weather { days, location, .. } => {
                *days = (*days).clamp(1, 7);
                *location = location.trim().to_owned();
            }
            Self::Cores { columns, .. } => {
                *columns = (*columns).clamp(0, MAX_ROWS);
            }
            Self::Disk {
                mounts,
                max_rows,
                ..
            } => {
                *mounts = mounts.trim().to_owned();
                *max_rows = (*max_rows).clamp(MIN_ROWS, MAX_ROWS);
            }
            Self::DiskIo { device, .. } | Self::Network { interface: device, .. } => {
                *device = device.trim().to_owned();
            }
            Self::Processes { count, .. } => {
                *count = (*count).clamp(MIN_ROWS, MAX_ROWS);
            }
        }
    }
}

/// How the number is read and shown, shared by the numeric views.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Number {
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default = "default_decimals")]
    pub decimals: usize,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub warn_at: Option<f64>,
}

impl Default for Number {
    fn default() -> Self {
        Self {
            scale: 1.0,
            offset: 0.0,
            decimals: 0,
            min: None,
            max: None,
            warn_at: None,
        }
    }
}

impl Number {
    fn normalise(&mut self) {
        if !self.scale.is_finite() {
            self.scale = 1.0;
        }
        if !self.offset.is_finite() {
            self.offset = 0.0;
        }
        self.decimals = self.decimals.min(MAX_DECIMALS);
        self.min = self.min.filter(|value| value.is_finite());
        self.max = self.max.filter(|value| value.is_finite());
        // A warning threshold only means something below the top of the scale.
        if let (Some(warn_at), Some(max)) = (self.warn_at, self.max) {
            if warn_at > max {
                self.warn_at = None;
            }
        }
    }
}

/// One setting a definition exposes in the detail view.
///
/// The `key` names a `[view]` field; changing the setting overrides that field
/// for the one tile. A definition that offers no settings is perfectly normal.
#[derive(Debug, Clone, Deserialize)]
pub struct Setting {
    /// The `[view]` key this setting overrides, e.g. `rows` or `label`.
    pub key: String,
    pub kind: SettingKind,
    pub title: String,
    /// Lower bound for an integer setting.
    #[serde(default)]
    pub min: Option<f64>,
    /// Upper bound for an integer setting.
    #[serde(default)]
    pub max: Option<f64>,
}

/// The data type of a [`Setting`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    Bool,
    Int,
    Text,
}

/// One plugin definition, i.e. the contents of a `*.toml` file.
#[derive(Debug, Clone, Deserialize)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default)]
    pub category: Category,
    /// Hide the tile while the source produces nothing.
    #[serde(default)]
    pub hidden_when_idle: bool,
    #[serde(default = "default_size")]
    pub size: (i32, i32),
    /// Seconds between samples, at least one.
    #[serde(default = "default_refresh")]
    pub refresh: u64,
    #[serde(default)]
    pub aspect: Option<f64>,
    /// Free form metadata, for definitions that are shared.
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    /// The settings the definition lets the user change in the detail view.
    #[serde(default)]
    pub settings: Vec<Setting>,
    /// A view that draws something of its own (a heading, a clock) may omit it.
    #[serde(default)]
    pub source: Option<Source>,
    pub view: View,
}

impl Plugin {
    /// Trims and clamps the hand written fields, so everything downstream can
    /// trust them, and reports the first problem found.
    fn normalise(&mut self) -> Result<(), String> {
        let id = self.id.trim().to_owned();
        check_id(&id)?;
        self.id = id;

        self.name = self.name.trim().to_owned();
        if self.name.is_empty() {
            return Err("name を指定してください".to_owned());
        }

        self.summary = self.summary.trim().to_owned();
        if self.icon.trim().is_empty() {
            self.icon = default_icon();
        }

        self.size = (
            self.size.0.clamp(MIN_SIDE, MAX_SIDE),
            self.size.1.clamp(MIN_SIDE, MAX_SIDE),
        );
        self.refresh = self.refresh.clamp(MIN_REFRESH, MAX_REFRESH);
        self.aspect = self
            .aspect
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0);
        self.author = self.author.trim().to_owned();
        self.version = self.version.trim().to_owned();
        self.homepage = self.homepage.trim().to_owned();
        self.license = self.license.trim().to_owned();
        for setting in &mut self.settings {
            setting.key = setting.key.trim().to_owned();
            setting.title = setting.title.trim().to_owned();
        }
        self.settings.retain(|setting| !setting.key.is_empty());
        if let Some(source) = &mut self.source {
            source.normalise()?;
        }
        self.view.normalise();
        Ok(())
    }

    /// A copy of this definition with the instance's `overrides` applied to its
    /// `[view]` table.
    ///
    /// This is how a user changes what a definition exposes — which sensors a
    /// bar chart lists, how a readout is labelled — without editing the file. A
    /// value that does not fit the view is dropped and the definition's own
    /// value stays, so a broken setting can never do more than fail to apply.
    pub fn with_overrides(&self, overrides: &serde_json::Map<String, serde_json::Value>) -> Plugin {
        if overrides.is_empty() {
            return self.clone();
        }
        let Ok(mut view) = serde_json::to_value(&self.view) else {
            return self.clone();
        };
        let Some(table) = view.as_object_mut() else {
            return self.clone();
        };
        for (key, value) in overrides {
            table.insert(key.clone(), value.clone());
        }
        let mut plugin = self.clone();
        match serde_json::from_value::<View>(view) {
            Ok(overridden) => {
                plugin.view = overridden;
                plugin.view.normalise();
            }
            Err(e) => warn!("{} の設定を適用できません: {e}", self.id),
        }
        plugin
    }
}

/// Sections of the widget picker the plugin shows up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    System,
    Time,
    #[default]
    Info,
    Apps,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "システム",
            Self::Time => "時刻と日付",
            Self::Info => "情報",
            Self::Apps => "アプリ",
        }
    }
}

/// A plugin that was loaded, either from disk or from the compiled-in set.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub plugin: Plugin,
    pub path: PathBuf,
    /// True when the definition ships with the application (see [`BUILTIN`]).
    pub builtin: bool,
}

/// The directory the plugin files live in, created if needed.
///
/// `$XDG_CONFIG_HOME/linux-easy-dashboard/plugins`, falling back to
/// `$HOME/.config/...` when that variable is unset, empty or relative.
pub fn directory() -> PathBuf {
    let dir = directory_path();
    if let Err(e) = fs::create_dir_all(&dir) {
        warn!("{} を作成できません: {e}", dir.display());
    }
    dir
}

fn directory_path() -> PathBuf {
    config_home().join(APP_DIR).join(PLUGIN_DIR)
}

fn config_home() -> PathBuf {
    if let Some(setting) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(setting);
        if path.is_absolute() {
            return path;
        }
    }
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(".config"),
        _ => PathBuf::from(".config"),
    }
}

thread_local! {
    /// The loaded plugins, filled on first use and replaced by [`reload`].
    static CATALOGUE: RefCell<Option<Vec<Loaded>>> = const { RefCell::new(None) };
    /// Bumped on every [`reload`], so tiles can notice that a definition they
    /// were built from may have changed and ask to be rebuilt.
    static REVISION: Cell<u64> = const { Cell::new(0) };
}

/// How many times the catalogue has been re-read.
pub fn revision() -> u64 {
    REVISION.with(Cell::get)
}

/// Widget definitions the application ships with.
///
/// These are ordinary plugins: the only difference is that they are compiled in
/// instead of living in the user's `plugins/` directory. Replacing a built-in
/// widget with a plugin is therefore a matter of writing a `.toml` here and
/// deleting the old Rust module — the kernel itself does not change.
const BUILTIN: &[&str] = &[
    include_str!("../plugins/builtin/analog_clock.toml"),
    include_str!("../plugins/builtin/battery.toml"),
    include_str!("../plugins/builtin/calendar.toml"),
    include_str!("../plugins/builtin/clock.toml"),
    include_str!("../plugins/builtin/cores.toml"),
    include_str!("../plugins/builtin/cpu.toml"),
    include_str!("../plugins/builtin/date.toml"),
    include_str!("../plugins/builtin/disk.toml"),
    include_str!("../plugins/builtin/diskio.toml"),
    include_str!("../plugins/builtin/heading.toml"),
    include_str!("../plugins/builtin/launcher.toml"),
    include_str!("../plugins/builtin/load.toml"),
    include_str!("../plugins/builtin/media.toml"),
    include_str!("../plugins/builtin/memory.toml"),
    include_str!("../plugins/builtin/network.toml"),
    include_str!("../plugins/builtin/note.toml"),
    include_str!("../plugins/builtin/processes.toml"),
    include_str!("../plugins/builtin/system.toml"),
    include_str!("../plugins/builtin/systeminfo.toml"),
    include_str!("../plugins/builtin/temperature.toml"),
    include_str!("../plugins/builtin/weather.toml"),
    include_str!("../plugins/builtin/world_clock.toml"),
];

/// The built-in definitions, parsed. A broken one is skipped with a warning, so
/// a typo cannot take the application down.
fn builtin_plugins() -> Vec<Loaded> {
    BUILTIN
        .iter()
        .filter_map(|text| match parse(text) {
            Ok(plugin) => {
                let path = PathBuf::from(format!("<builtin>/{}.{EXTENSION}", plugin.id));
                Some(Loaded {
                    plugin,
                    path,
                    builtin: true,
                })
            }
            Err(message) => {
                warn!("組み込みウィジェットを読み込めません: {message}");
                None
            }
        })
        .collect()
}

/// Built-in definitions first, then the user's own, so a definition that ships
/// with the app cannot be shadowed by accident.
fn load_catalogue() -> Vec<Loaded> {
    let mut loaded = builtin_plugins();
    loaded.extend(load_all(&directory()));
    loaded
}

/// Every plugin that loads: the built-in ones, then the user's, by name.
pub fn catalogue() -> Vec<Loaded> {
    CATALOGUE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(load_catalogue());
        }
        match slot.as_ref() {
            Some(loaded) => loaded.clone(),
            None => Vec::new(),
        }
    })
}

/// One plugin by id.
pub fn find(id: &str) -> Option<Loaded> {
    catalogue()
        .into_iter()
        .find(|loaded| loaded.plugin.id == id)
}

/// Whether `id` names a definition that ships with the application.
pub fn is_builtin(id: &str) -> bool {
    catalogue()
        .iter()
        .any(|loaded| loaded.builtin && loaded.plugin.id == id)
}

/// Re-reads the directory and returns how many user plugins loaded. Files that
/// do not parse are skipped with a warning.
pub fn reload() -> usize {
    let mut loaded = builtin_plugins();
    let user = load_all(&directory());
    let count = user.len();
    loaded.extend(user);
    CATALOGUE.with(|cell| *cell.borrow_mut() = Some(loaded));
    REVISION.with(|cell| cell.set(cell.get().wrapping_add(1)));
    info!("プラグインを {count} 件読み込みました");
    count
}

/// The file behind `id`, for the in-app editor.
pub fn read_source(id: &str) -> Option<String> {
    read_source_in(&directory(), id)
}

/// Writes the file and reloads.
///
/// Text that does not parse is still written — the user has to be able to fix it
/// in the editor — and the parse error is returned so the UI can show it.
pub fn write_source(id: &str, text: &str) -> Result<(), String> {
    if is_builtin(id) {
        return Err("組み込みウィジェットは編集できません".to_owned());
    }
    let result = write_source_in(&directory(), id, text);
    reload();
    result
}

/// Deletes the file and reloads.
pub fn remove(id: &str) -> Result<(), String> {
    if is_builtin(id) {
        return Err("組み込みウィジェットは削除できません".to_owned());
    }
    let result = remove_in(&directory(), id);
    reload();
    result
}

fn read_source_in(dir: &Path, id: &str) -> Option<String> {
    if !is_valid_id(id) {
        return None;
    }
    fs::read_to_string(target_path(dir, id)).ok()
}

fn write_source_in(dir: &Path, id: &str, text: &str) -> Result<(), String> {
    if !is_valid_id(id) {
        return Err(format!("プラグイン ID が不正です: {id}"));
    }
    let path = target_path(dir, id);
    fs::create_dir_all(dir).map_err(|e| format!("{} を作成できません: {e}", dir.display()))?;
    // Same dance as the layout file: a temp file plus a rename, so a crash can
    // never leave half a definition behind.
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    fs::write(&tmp, text.as_bytes()).map_err(|e| format!("書き込めません: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("置き換えできません: {e}")
    })?;
    parse(text).map(|_| ())
}

fn remove_in(dir: &Path, id: &str) -> Result<(), String> {
    if !is_valid_id(id) {
        return Err(format!("プラグイン ID が不正です: {id}"));
    }
    let path = target_path(dir, id);
    if !path.starts_with(dir) {
        return Err(format!("削除できません: {}", path.display()));
    }
    fs::remove_file(&path).map_err(|e| format!("{} を削除できません: {e}", path.display()))
}

/// Where `id` lives: the file it was loaded from, else a new `{id}.toml`.
fn target_path(dir: &Path, id: &str) -> PathBuf {
    load_all(dir)
        .into_iter()
        .find(|loaded| loaded.plugin.id == id)
        .map_or_else(
            || dir.join(format!("{id}.{EXTENSION}")),
            |loaded| loaded.path,
        )
}

fn is_valid_id(id: &str) -> bool {
    check_id(id).is_ok()
}

/// An id is both a key and a file name, so only `[a-z0-9-]` is allowed.
fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("id を指定してください".to_owned());
    }
    if id.chars().count() > MAX_ID_LEN {
        return Err(format!("id は {MAX_ID_LEN} 文字までです"));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return Err(format!("id に使える文字は [a-z0-9_-] だけです: {id}"));
    }
    Ok(())
}

/// Parses and validates one plugin document.
pub fn parse(text: &str) -> Result<Plugin, String> {
    let mut plugin: Plugin = toml::from_str(text).map_err(|e| e.to_string())?;
    plugin.normalise()?;
    Ok(plugin)
}

/// Every `*.toml` in `dir`, sorted by name. Unreadable and broken files are
/// skipped with a warning, the way `procfs` treats files it cannot parse.
fn load_all(dir: &Path) -> Vec<Loaded> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!("{} を読めません: {e}", dir.display());
            return Vec::new();
        }
    };

    let mut loaded = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_toml = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION));
        if !is_toml || !path.is_file() {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                warn!("{} を読めません: {e}", path.display());
                continue;
            }
        };
        match parse(&text) {
            Ok(plugin) => loaded.push(Loaded {
                plugin,
                path,
                builtin: false,
            }),
            Err(message) => warn!("{} を読み込めません: {message}", path.display()),
        }
    }

    loaded.sort_by(|a, b| {
        a.plugin
            .name
            .cmp(&b.plugin.name)
            .then_with(|| a.plugin.id.cmp(&b.plugin.id))
    });
    loaded
}

/// Runs the source and returns its raw output. Blocking on purpose: the caller
/// is expected to be a worker thread.
///
/// Trailing whitespace is dropped and an empty result means "nothing right now".
/// A command that exits non-zero still reports its output; only when there is
/// none does the error surface. A file that cannot be read is an error too.
pub fn sample(source: &Source) -> Result<String, String> {
    match source {
        Source::Command { run, timeout_secs } => run_command(run, *timeout_secs),
        Source::File { path } => read_file(path),
        Source::Clock { format, timezone } => sample_clock(format, timezone),
        Source::Http {
            url,
            timeout_secs,
            json_pointer,
        } => sample_http(url, *timeout_secs, json_pointer),
    }
}

fn sample_clock(format: &str, timezone: &str) -> Result<String, String> {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| format!("時刻を読めません: {e}"))?;
    let zone = if timezone.trim().is_empty() {
        "local".to_owned()
    } else {
        timezone.trim().to_owned()
    };
    let epoch = now.as_secs() as i64;
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg("date -d \"@$1\" \"$2\"")
        .arg("--")
        .arg(epoch.to_string())
        .arg(format)
        .env("TZ", &zone)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("時刻を整形できません: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("出力を読めません: {e}"))?;
    let text = truncate(String::from_utf8_lossy(&output.stdout).trim_end(), MAX_OUTPUT);
    if text.is_empty() {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    } else {
        Ok(text)
    }
}

fn sample_http(url: &str, timeout_secs: u64, json_pointer: &str) -> Result<String, String> {
    let response = minreq::get(url)
        .with_timeout(timeout_secs.clamp(MIN_TIMEOUT, MAX_TIMEOUT))
        .send()
        .map_err(|e| format!("取得できません: {e}"))?;
    let body = truncate(
        String::from_utf8_lossy(response.as_bytes()).trim_end(),
        FILE_LIMIT as usize,
    );
    if json_pointer.trim().is_empty() {
        return Ok(truncate(&body, MAX_OUTPUT));
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON を読めません: {e}"))?;
    let mut cursor = &value;
    for raw in json_pointer.trim().split('/').skip(1) {
        let key = raw.replace("~1", "/").replace("~0", "~");
        cursor = match cursor {
            serde_json::Value::Object(map) => map
                .get(&key)
                .ok_or_else(|| format!("JSON ポインタが指すキーがありません: {key}"))?,
            serde_json::Value::Array(list) => {
                let index: usize = key
                    .parse()
                    .map_err(|_| format!("JSON ポインタの配列指定が不正です: {key}"))?;
                list.get(index)
                    .ok_or_else(|| format!("JSON ポインタの配列範囲外です: {key}"))?
            }
            _ => return Err("JSON ポインタの途中がオブジェクトでも配列でもありません".to_owned()),
        };
    }
    Ok(match cursor {
        serde_json::Value::String(text) => truncate(text, MAX_OUTPUT),
        serde_json::Value::Null => String::new(),
        other => truncate(&other.to_string(), MAX_OUTPUT),
    })
}

fn run_command(run: &str, timeout_secs: u64) -> Result<String, String> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(run)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("コマンドを起動できません: {e}"))?;

    let timeout = Duration::from_secs(timeout_secs.clamp(MIN_TIMEOUT, MAX_TIMEOUT));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            // A command that writes forever would otherwise hang the worker: it
            // fills the pipe, never exits, and this is the way out.
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} 秒でタイムアウトしました", timeout.as_secs()));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("実行状態を確認できません: {e}"));
            }
        }
    };

    let output = child
        .wait_with_output()
        .map_err(|e| format!("出力を読めません: {e}"))?;
    let stdout = truncate(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        MAX_OUTPUT,
    );
    if !stdout.is_empty() {
        return Ok(stdout);
    }
    if status.success() {
        return Ok(String::new());
    }
    let stderr = truncate(
        String::from_utf8_lossy(&output.stderr).trim_end(),
        MAX_OUTPUT,
    );
    if stderr.is_empty() {
        Err(describe_failure(status))
    } else {
        Err(stderr)
    }
}

fn describe_failure(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("コマンドが終了コード {code} で失敗しました"),
        None => "コマンドがシグナルで終了しました".to_owned(),
    }
}

fn read_file(path: &str) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| format!("{path} を開けません: {e}"))?;
    let mut buffer = Vec::new();
    file.take(FILE_LIMIT)
        .read_to_end(&mut buffer)
        .map_err(|e| format!("{path} を読めません: {e}"))?;
    Ok(truncate(
        String::from_utf8_lossy(&buffer).trim_end(),
        MAX_OUTPUT,
    ))
}

/// Cuts `text` down to at most `limit` bytes without splitting a character.
fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut out = String::with_capacity(limit);
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > limit {
            break;
        }
        out.push(ch);
    }
    out
}

/// The first number in `text` after `scale` and `offset` are applied.
pub fn number_of(plugin: &Plugin, text: &str) -> Option<f64> {
    let number = plugin.view.number()?;
    let raw = first_number(text)?;
    Some(raw * number.scale + number.offset)
}

/// `value` as a fraction of `min..=max`.
///
/// An unset bound falls back to `0..100`, so a gauge without explicit limits
/// behaves like a percentage. Values outside the range are clamped.
pub fn fraction(plugin: &Plugin, value: f64) -> f64 {
    let number = plugin.view.number().unwrap_or_default();
    let min = number.min.unwrap_or(0.0);
    let max = number.max.unwrap_or(100.0);
    let span = max - min;
    if !span.is_finite() || span <= 0.0 {
        return 0.0;
    }
    let ratio = ((value - min) / span).clamp(0.0, 1.0);
    if ratio.is_finite() { ratio } else { 0.0 }
}

/// `value` rounded to `decimals`, with the unit appended, e.g. `48 °C`.
pub fn format_value(plugin: &Plugin, value: f64) -> String {
    let decimals = plugin
        .view
        .number()
        .unwrap_or_default()
        .decimals
        .min(MAX_DECIMALS);
    let digits = format!("{value:.decimals$}", decimals = decimals);
    let unit = plugin.view.unit().trim();
    if unit.is_empty() {
        digits
    } else {
        format!("{digits} {unit}")
    }
}

/// The rows of a list view: every line split into a name and a value.
///
/// Lines without the split string become a name with an empty value, and at
/// most `rows` of them are returned, so a command that prints a lot cannot make
/// the tile grow.
///
/// The number of rows the *tile* shows is fixed when it is built; the view asks
/// for exactly that many.
pub fn rows_of(plugin: &Plugin, text: &str) -> Vec<(String, String)> {
    let Some((rows, ..)) = row_settings(plugin) else {
        return Vec::new();
    };
    split_lines(plugin, text).into_iter().take(rows).collect()
}

/// The rows of a `bars` view: a label and the number found in its value column.
///
/// A row whose value has no number in it is dropped, so a command that prints a
/// stray line cannot turn into an empty bar. The definition's `filter` and
/// `sort` are applied before the list is cut to `rows`.
pub fn bar_rows(plugin: &Plugin, text: &str) -> Vec<(String, f64)> {
    let Some((rows, ..)) = row_settings(plugin) else {
        return Vec::new();
    };
    let (filter, sort) = match &plugin.view {
        View::Bars { filter, sort, .. } => (filter.trim().to_owned(), sort.trim().to_owned()),
        _ => return Vec::new(),
    };
    let number = plugin.view.number().unwrap_or_default();
    let mut values: Vec<(String, f64)> = split_lines(plugin, text)
        .into_iter()
        .filter(|(label, _)| filter.is_empty() || label.contains(&filter))
        .filter_map(|(label, value)| {
            let raw = first_number(&value)?;
            Some((label, raw * number.scale + number.offset))
        })
        .collect();
    match sort.as_str() {
        "value" => values.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))),
        "label" => values.sort_by(|a, b| a.0.cmp(&b.0)),
        _ => {}
    }
    values.truncate(rows);
    values
}

/// The `rows` / `split` / `label_column` / `value_column` of a row oriented
/// view, or `None` when the view has no rows at all.
fn row_settings(plugin: &Plugin) -> Option<(usize, String, usize, usize)> {
    match &plugin.view {
        View::List {
            rows,
            split,
            label_column,
            value_column,
        }
        | View::Facts {
            rows,
            split,
            label_column,
            value_column,
        }
        | View::Bars {
            rows,
            split,
            label_column,
            value_column,
            ..
        } => Some((*rows, split.clone(), *label_column, *value_column)),
        _ => None,
    }
}

/// Every non-blank line split into a `(label, value)` pair, without limiting the
/// count: the callers decide how many they want.
fn split_lines(plugin: &Plugin, text: &str) -> Vec<(String, String)> {
    let Some((_, split, label_column, value_column)) = row_settings(plugin) else {
        return Vec::new();
    };

    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = if split.is_empty() {
                vec![line]
            } else {
                line.split(split.as_str()).collect()
            };
            let label = fields.get(label_column).copied().unwrap_or(line).trim();
            let value = fields
                .get(value_column)
                .copied()
                .filter(|value| *value != label)
                .unwrap_or("")
                .trim();
            (label.to_owned(), value.to_owned())
        })
        .collect()
}

/// The first `-?[0-9]+(\.[0-9]+)?` in `text`, parsed.
fn first_number(text: &str) -> Option<f64> {
    /// Where the scanner is inside a candidate number.
    enum State {
        /// Looking for a sign or a digit.
        Seek,
        /// Saw a `-`, waiting for a digit.
        Sign,
        /// Reading the integer part.
        Integer,
        /// Saw the decimal point, waiting for a digit.
        Dot,
        /// Reading the fraction part.
        Fraction,
    }

    let mut state = State::Seek;
    let mut found = String::new();
    for ch in text.chars() {
        match state {
            State::Seek => {
                if ch == '-' {
                    state = State::Sign;
                } else if ch.is_ascii_digit() {
                    found.push(ch);
                    state = State::Integer;
                }
            }
            State::Sign => {
                if ch.is_ascii_digit() {
                    found.push('-');
                    found.push(ch);
                    state = State::Integer;
                } else if ch != '-' {
                    state = State::Seek;
                }
            }
            State::Integer => {
                if ch.is_ascii_digit() {
                    found.push(ch);
                } else if ch == '.' {
                    state = State::Dot;
                } else {
                    return found.parse().ok();
                }
            }
            State::Dot => {
                if ch.is_ascii_digit() {
                    found.push('.');
                    found.push(ch);
                    state = State::Fraction;
                } else {
                    // `12.` is just `12`.
                    return found.parse().ok();
                }
            }
            State::Fraction => {
                if ch.is_ascii_digit() {
                    found.push(ch);
                } else {
                    return found.parse().ok();
                }
            }
        }
    }

    match state {
        State::Integer | State::Fraction => found.parse().ok(),
        _ => None,
    }
}

fn default_timeout() -> u64 {
    5
}

fn default_history() -> usize {
    60
}

fn default_rows() -> usize {
    6
}

fn default_split() -> String {
    "\t".to_owned()
}

fn default_value_column() -> usize {
    1
}

fn default_true() -> bool {
    true
}

fn default_decimals() -> usize {
    0
}

fn one() -> f64 {
    1.0
}

fn default_icon() -> String {
    "application-x-executable-symbolic".to_owned()
}

fn default_columns() -> usize {
    3
}

fn default_days() -> usize {
    3
}

fn default_disk_path() -> String {
    "/".to_owned()
}


fn default_size() -> (i32, i32) {
    (300, 180)
}

fn default_refresh() -> u64 {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring gauge fed by a file, with every number field spelled out.
    const RING: &str = r#"
id = "cpu-freq"
name = "CPU 周波数"
summary = "scaling_cur_freq を読む"
refresh = 2
hidden_when_idle = true

[source]
kind = "file"
path = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq"

[view]
kind = "ring"
min = 0
max = 5000
scale = 0.001
unit = "MHz"
warn_at = 4000
"#;

    /// The smallest useful document: everything else takes defaults.
    const MINIMAL: &str = r#"
id = "tiny"
name = "ちいさい"

[source]
kind = "command"
run = "printf 7"

[view]
kind = "readout"
"#;

    const LIST: &str = r#"
id = "git-dirty"
name = "変更"

[source]
kind = "command"
run = "git status --porcelain"

[view]
kind = "list"
rows = 3
split = "|"
label_column = 1
value_column = 0
"#;

    fn def(text: &str) -> Plugin {
        parse(text).expect("the test document must parse")
    }

    #[test]
    fn a_ring_definition_keeps_what_the_user_wrote() {
        let plugin = def(RING);
        assert_eq!(plugin.id, "cpu-freq");
        assert_eq!(plugin.name, "CPU 周波数");
        assert_eq!(plugin.summary, "scaling_cur_freq を読む");
        assert_eq!(plugin.refresh, 2);
        assert!(plugin.hidden_when_idle);
        assert!(matches!(
            &plugin.source,
            Some(Source::File { path }) if path.contains("scaling_cur_freq")
        ));
        assert!(matches!(plugin.view, View::Ring { .. }));
        assert_eq!(plugin.view.unit(), "MHz");

        let number = plugin.view.number().expect("a ring is a number view");
        assert_eq!(number.min, Some(0.0));
        assert_eq!(number.max, Some(5000.0));
        assert_eq!(number.warn_at, Some(4000.0));
        assert!((number.scale - 0.001).abs() < f64::EPSILON);
    }

    #[test]
    fn a_minimal_definition_falls_back_to_sensible_defaults() {
        let plugin = def(MINIMAL);
        assert_eq!(plugin.size, (300, 180));
        assert_eq!(plugin.refresh, 5);
        assert_eq!(plugin.icon, "application-x-executable-symbolic");
        assert_eq!(plugin.category, Category::Info);
        assert!(!plugin.hidden_when_idle);
        assert!(plugin.summary.is_empty());
        let number = plugin.view.number().expect("a readout is a number view");
        assert_eq!(number.decimals, 0);
        assert!((number.scale - 1.0).abs() < f64::EPSILON);
        assert_eq!(number.min, None);
    }

    #[test]
    fn a_list_definition_keeps_its_columns() {
        let plugin = def(LIST);
        let View::List {
            rows,
            split,
            label_column,
            value_column,
        } = &plugin.view
        else {
            panic!("expected a list view");
        };
        assert_eq!((*rows, split.as_str()), (3, "|"));
        assert_eq!((*label_column, *value_column), (1, 0));
    }

    #[test]
    fn broken_documents_are_reported_instead_of_loaded() {
        // Unknown key, so the file is a typo rather than a definition.
        let cases = [
            "name = \"x\"\n[source]\nkind = \"command\"\nrun = \"true\"\n[view]\nkind = \"text\"\n",
            "id = \"a\"\nid = \"b\"\n",
            "id = \"Bad Id\"\nname = \"x\"\n[source]\nkind = \"command\"\nrun = \"true\"\n[view]\nkind = \"text\"\n",
            "id = \"a\"\nname = \"x\"\n[source]\nkind = \"command\"\nrun = \"true\"\n[view]\nkind = \"gauge\"\n",
            "id = \"a\"\nname = \"x\"\n[source]\nkind = \"watson\"\n[view]\nkind = \"text\"\n",
            "id = \"a\"\nname = \"x\"\n[view]\nkind = \"gauge\"\n",
            "",
        ];
        for case in cases {
            // An empty string is not even valid TOML-less input: `parse` must
            // still say no instead of panicking.
            assert!(parse(case).is_err(), "this must not load: {case:?}");
        }
    }

    #[test]
    fn a_definition_may_leave_out_the_source() {
        // A view that draws something of its own needs no value, so `[source]`
        // is optional (this is what lets a heading or a clock be a plugin).
        let plugin = parse("id = \"a\"\nname = \"見出し\"\n[view]\nkind = \"text\"\n")
            .expect("a source-less definition must parse");
        assert!(plugin.source.is_none());
    }

    #[test]
    fn settings_override_the_view_but_never_break_it() {
        let plugin = def(LIST);
        let mut overrides = serde_json::Map::new();
        overrides.insert("rows".to_owned(), serde_json::json!(1));
        let narrowed = plugin.with_overrides(&overrides);
        assert!(matches!(&narrowed.view, View::List { rows: 1, .. }));
        // The definition itself is untouched.
        assert!(matches!(&plugin.view, View::List { rows: 3, .. }));

        // A value that does not fit is ignored, leaving the definition's own.
        let mut broken = serde_json::Map::new();
        broken.insert("rows".to_owned(), serde_json::json!("many"));
        let unchanged = plugin.with_overrides(&broken);
        assert!(matches!(&unchanged.view, View::List { rows: 3, .. }));
    }

    #[test]
    fn numbers_are_picked_out_of_typical_output() {
        let plugin = def(RING); // scale 0.001
        assert_eq!(number_of(&plugin, "3800000\n"), Some(3800.0));
        assert_eq!(number_of(&plugin, "-12345"), Some(-12.345));
        assert_eq!(number_of(&plugin, "気温 48.5°C"), Some(0.0485));
        assert_eq!(number_of(&plugin, "no digits here"), None);
        assert_eq!(number_of(&plugin, ""), None);

        // A view without number settings (a list) never reports a number.
        assert_eq!(number_of(&def(LIST), "1.5"), None);
    }

    #[test]
    fn fractions_are_clamped_to_the_declared_range() {
        let plugin = def(RING); // 0..=5000
        assert_eq!(fraction(&plugin, 0.0), 0.0);
        assert_eq!(fraction(&plugin, 2500.0), 0.5);
        assert_eq!(fraction(&plugin, 9999.0), 1.0);
        assert_eq!(fraction(&plugin, -5.0), 0.0);

        // Without limits the scale is a percentage.
        let percent = def(MINIMAL);
        assert_eq!(fraction(&percent, 30.0), 0.3);
        assert_eq!(fraction(&percent, 500.0), 1.0);
    }

    #[test]
    fn values_carry_their_unit() {
        let plugin = def(RING);
        assert_eq!(format_value(&plugin, 3800.0), "3800 MHz");

        let bare = def(MINIMAL);
        assert_eq!(format_value(&bare, 7.4), "7");
    }

    #[test]
    fn list_rows_split_into_names_and_values() {
        let plugin = def(LIST);
        let text = "M file.txt|3\n?? new.rs|0\nno separator here\nM other.rs|1\n";
        assert_eq!(
            rows_of(&plugin, text),
            vec![
                ("3".to_owned(), "M file.txt".to_owned()),
                ("0".to_owned(), "?? new.rs".to_owned()),
                ("no separator here".to_owned(), String::new()),
            ]
        );

        // Empty output, and output that is only blank lines, mean "no rows".
        assert!(rows_of(&plugin, "").is_empty());
        assert!(rows_of(&plugin, "\n\n").is_empty());
        // Other views have no rows.
        assert!(rows_of(&def(RING), "a|b").is_empty());
    }

    #[test]
    fn commands_and_files_can_be_sampled() {
        let source = Source::Command {
            run: "printf 42".to_owned(),
            timeout_secs: 5,
        };
        assert_eq!(sample(&source).as_deref(), Ok("42"));

        // Nothing printed is the "nothing to show" case, not an error.
        let quiet = Source::Command {
            run: "true".to_owned(),
            timeout_secs: 5,
        };
        assert_eq!(sample(&quiet).as_deref(), Ok(""));

        // A failing command with output still reports the output.
        let noisy = Source::Command {
            run: "printf 3; exit 1".to_owned(),
            timeout_secs: 5,
        };
        assert_eq!(sample(&noisy).as_deref(), Ok("3"));

        // A failing command without output reports the failure.
        let failing = Source::Command {
            run: "exit 3".to_owned(),
            timeout_secs: 5,
        };
        assert!(sample(&failing).is_err());

        // Files: the contents, trimmed.
        let dir = temp_dir("sample");
        let path = dir.join("value.txt");
        fs::write(&path, "48000\n").expect("the temp file must be writable");
        let file = Source::File {
            path: path.to_string_lossy().to_string(),
        };
        assert_eq!(sample(&file).as_deref(), Ok("48000"));

        let missing = Source::File {
            path: dir.join("nope.txt").to_string_lossy().to_string(),
        };
        assert!(sample(&missing).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sources_round_trip_through_the_plugins_directory() {
        let dir = temp_dir("roundtrip");
        assert_eq!(write_source_in(&dir, "tiny", MINIMAL), Ok(()));
        assert_eq!(read_source_in(&dir, "tiny").as_deref(), Some(MINIMAL));

        // The file is written even when it does not parse yet, so the user can
        // carry on fixing it, but the problem is reported.
        assert!(write_source_in(&dir, "tiny", "id = \"tiny\"\n").is_err());
        assert_eq!(
            read_source_in(&dir, "tiny").as_deref(),
            Some("id = \"tiny\"\n")
        );

        // Ids are file names, so they are checked before touching the disk.
        assert!(write_source_in(&dir, "../escape", MINIMAL).is_err());
        assert!(remove_in(&dir, "../escape").is_err());

        assert_eq!(remove_in(&dir, "tiny"), Ok(()));
        assert!(read_source_in(&dir, "tiny").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_machine_has_a_plugin_directory() {
        assert!(directory().ends_with(PLUGIN_DIR));
    }

    #[test]
    fn the_built_in_definitions_all_load() {
        let builtins = builtin_plugins();
        assert_eq!(
            builtins.len(),
            BUILTIN.len(),
            "組み込み定義のどれかが読み込めません"
        );
        let ids: Vec<&str> = builtins
            .iter()
            .map(|loaded| loaded.plugin.id.as_str())
            .collect();
        // The migrated widgets kept their kind, so an old layout still resolves.
        assert!(ids.contains(&"temperature"));
        assert!(ids.contains(&"systeminfo"));
        for loaded in &builtins {
            assert!(loaded.builtin);
            assert!(!loaded.plugin.name.is_empty());
            assert!(find(&loaded.plugin.id).is_some());
        }
    }

    #[test]
    fn a_bars_view_filters_and_sorts_before_it_is_cut_to_rows() {
        let plugin = parse(
            "id = \"t\"\nname = \"t\"\n[source]\nkind = \"command\"\nrun = \"true\"\n\
             [view]\nkind = \"bars\"\nrows = 2\nsplit = \"\\t\"\nsort = \"value\"\n",
        )
        .expect("the test definition must parse");
        // Deliberately out of order: the two largest survive, largest first.
        let rows = bar_rows(&plugin, "a\t10\nb\t40\nc\t30\n");
        assert_eq!(rows, vec![("b".to_owned(), 40.0), ("c".to_owned(), 30.0)]);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "edm-plugin-{}-{name}-{}",
            std::process::id(),
            REVISION.with(Cell::get)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temp directory must be creatable");
        dir
    }
}
