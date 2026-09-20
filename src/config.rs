//! Persistent state: what the user has on their dashboard.
//!
//! Everything lives in a single human readable JSON document inside
//! `$XDG_CONFIG_HOME/easy-dashboard-maker/layout.json`. The file is written
//! atomically (temp file + `rename`) so a crash can never leave a half written
//! layout behind.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};

/// Directory below `$XDG_CONFIG_HOME`.
pub const APP_DIR: &str = "easy-dashboard-maker";
const LAYOUT_FILE: &str = "layout.json";
const BACKUP_FILE: &str = "layout.invalid.json";
const CONFIG_VERSION: u32 = 1;

/// How the canvas itself behaves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasSettings {
    /// Grid step and alignment guide distance, in logical pixels.
    pub grid: i32,
    /// Draw the alignment grid while editing.
    pub show_grid: bool,
    /// Snap to guides, the grid and the canvas edges.
    pub snap: bool,
}

impl Default for CanvasSettings {
    fn default() -> Self {
        Self {
            grid: 16,
            show_grid: true,
            snap: true,
        }
    }
}

/// Remembered window geometry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 1200,
            height: 780,
            maximized: false,
        }
    }
}

/// A widget that lives on the canvas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetInstance {
    pub id: String,
    /// Registry key, see [`crate::widgets::descriptors`].
    pub kind: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Keep the widget's natural aspect ratio while resizing.
    #[serde(default)]
    pub aspect_locked: bool,
    /// Widget specific settings, interpreted by the widget itself.
    #[serde(default)]
    pub config: serde_json::Value,
}

impl WidgetInstance {
    pub fn new(kind: impl Into<String>, x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            id: glib::uuid_string_random().to_string(),
            kind: kind.into(),
            x,
            y,
            w,
            h,
            aspect_locked: false,
            config: serde_json::Value::Null,
        }
    }
}

/// The whole persisted document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub window: WindowSettings,
    pub canvas: CanvasSettings,
    pub widgets: Vec<WidgetInstance>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            window: WindowSettings::default(),
            canvas: CanvasSettings::default(),
            widgets: Vec::new(),
        }
    }
}

impl AppConfig {
    /// The layout used on first start: a clock wall plus the most useful system
    /// readouts. The weather widget is deliberately *not* here — it is the only
    /// one that talks to the network, so it is added on request.
    pub fn starter() -> Self {
        use crate::widgets::{analog_clock, calendar, clock, cpu, memory, system};

        let mut analog = WidgetInstance::new(analog_clock::KIND, 400, 24, 240, 240);
        analog.aspect_locked = true;

        Self {
            window: WindowSettings {
                width: 1360,
                height: 840,
                maximized: false,
            },
            widgets: vec![
                WidgetInstance::new(clock::KIND, 24, 24, 360, 170),
                analog,
                WidgetInstance::new(calendar::KIND, 664, 24, 380, 280),
                WidgetInstance::new(cpu::KIND, 24, 215, 360, 240),
                WidgetInstance::new(memory::KIND, 400, 280, 320, 220),
                WidgetInstance::new(system::KIND, 24, 475, 360, 300),
            ],
            ..Self::default()
        }
    }
}

/// Reads and writes [`AppConfig`] in the XDG config directory.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// Resolves `$XDG_CONFIG_HOME/easy-dashboard-maker/layout.json`, creating
    /// the directory if needed.
    pub fn new() -> Result<Self> {
        let dir = glib::user_config_dir().join(APP_DIR);
        fs::create_dir_all(&dir)
            .with_context(|| format!("設定ディレクトリを作成できません: {}", dir.display()))?;
        Ok(Self {
            path: dir.join(LAYOUT_FILE),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the layout. `Ok(None)` means "nothing saved yet" (first start).
    ///
    /// A corrupt file is moved aside instead of being silently overwritten, so
    /// a hand edited layout can still be recovered.
    pub fn load(&self) -> Result<Option<AppConfig>> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("読み込めません: {}", self.path.display()));
            }
        };

        match serde_json::from_str::<AppConfig>(&raw) {
            Ok(config) => {
                info!(
                    "レイアウトを読み込みました ({} ウィジェット)",
                    config.widgets.len()
                );
                Ok(Some(config))
            }
            Err(e) => {
                let backup = self.path.with_file_name(BACKUP_FILE);
                if let Err(e) = fs::rename(&self.path, &backup) {
                    warn!("壊れたレイアウトを退避できません: {e}");
                } else {
                    warn!("壊れたレイアウトを {} に退避しました", backup.display());
                }
                Err(e.into())
            }
        }
    }

    /// Writes the layout atomically.
    pub fn save(&self, config: &AppConfig) -> Result<()> {
        let body =
            serde_json::to_string_pretty(config).context("レイアウトを JSON にできません")?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, body.as_bytes())
            .with_context(|| format!("書き込めません: {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("置き換えできません: {}", self.path.display()))?;
        Ok(())
    }
}
