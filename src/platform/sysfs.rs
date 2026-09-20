//! `/sys` readers: batteries and temperature sensors.
//!
//! Both are optional hardware. Every function returns an empty list when the
//! machine has none, and widgets render a quiet "not available" state instead
//! of failing.

use std::fs;
use std::path::{Path, PathBuf};

use log::debug;

const POWER_SUPPLY: &str = "/sys/class/power_supply";
const HWMON: &str = "/sys/class/hwmon";

/// Charge state reported by the firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryStatus {
    Charging,
    Discharging,
    Full,
    Unknown,
}

impl BatteryStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Charging => "充電中",
            Self::Discharging => "放電中",
            Self::Full => "満充電",
            Self::Unknown => "不明",
        }
    }

    pub fn is_charging(self) -> bool {
        matches!(self, Self::Charging | Self::Full)
    }
}

/// One battery, as exposed by the kernel.
#[derive(Debug, Clone)]
pub struct Battery {
    pub name: String,
    /// Charge in percent.
    pub capacity: f64,
    pub status: BatteryStatus,
    /// Estimated minutes until empty (or until full while charging).
    pub minutes: Option<u64>,
}

/// Every battery the kernel knows about.
pub fn batteries() -> Vec<Battery> {
    let Ok(entries) = fs::read_dir(POWER_SUPPLY) else {
        return Vec::new();
    };
    let mut batteries: Vec<Battery> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| read_trimmed(&path.join("type")).as_deref() == Some("Battery"))
        .filter_map(|path| read_battery(&path))
        .collect();
    batteries.sort_by(|a, b| a.name.cmp(&b.name));
    batteries
}

fn read_battery(path: &Path) -> Option<Battery> {
    let capacity = read_trimmed(&path.join("capacity"))?.parse::<f64>().ok()?;
    let status = match read_trimmed(&path.join("status")).as_deref() {
        Some("Charging") => BatteryStatus::Charging,
        Some("Discharging") => BatteryStatus::Discharging,
        Some("Full") => BatteryStatus::Full,
        _ => BatteryStatus::Unknown,
    };

    Some(Battery {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "BAT".to_owned()),
        capacity: capacity.clamp(0.0, 100.0),
        status,
        minutes: estimate_minutes(path, capacity, status),
    })
}

/// Battery time estimates are exposed as either energy/power or charge/current.
fn estimate_minutes(path: &Path, capacity: f64, status: BatteryStatus) -> Option<u64> {
    let (now, rate) = match (
        read_number(&path.join("energy_now")),
        read_number(&path.join("power_now")),
    ) {
        (Some(now), Some(rate)) if rate > 0.0 => (now, rate),
        _ => (
            read_number(&path.join("charge_now"))?,
            read_number(&path.join("current_now"))?,
        ),
    };
    if rate <= 0.0 {
        return None;
    }

    let full = match status {
        BatteryStatus::Charging => read_number(&path.join("energy_full"))
            .or_else(|| read_number(&path.join("charge_full")))?,
        _ => 0.0,
    };
    let remaining = if status.is_charging() {
        (full - now).max(0.0)
    } else {
        now
    };
    let _ = capacity;
    let minutes = (remaining / rate * 60.0).round();
    (minutes.is_finite() && minutes >= 0.0).then_some(minutes as u64)
}

/// A temperature sensor from `/sys/class/hwmon`.
#[derive(Debug, Clone)]
pub struct Sensor {
    /// Driver name, e.g. `coretemp`.
    pub chip: String,
    /// Human readable label, e.g. `Package id 0`.
    pub label: String,
    pub celsius: f64,
}

/// All plausible temperature sensors, hottest first.
pub fn temperature_sensors() -> Vec<Sensor> {
    let Ok(entries) = fs::read_dir(HWMON) else {
        return Vec::new();
    };

    let mut sensors = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        let chip = read_trimmed(&path.join("name")).unwrap_or_else(|| "hwmon".to_owned());
        for input in files_named(&path, "temp", "_input") {
            let Some(celsius) = read_number(&input).map(|raw| raw / 1000.0) else {
                continue;
            };
            // Sensors can report 0 or nonsense when the hardware is idle or
            // unsupported; keep only what looks like a real temperature.
            if !(1.0..150.0).contains(&celsius) {
                continue;
            }
            let label_path = input.with_file_name(
                input
                    .file_stem()
                    .map(|stem| format!("{}_label", stem.to_string_lossy()))
                    .unwrap_or_default(),
            );
            let label = read_trimmed(&label_path).unwrap_or_else(|| chip.clone());
            sensors.push(Sensor {
                chip: chip.clone(),
                label,
                celsius,
            });
        }
    }

    sensors.sort_by(|a, b| {
        b.celsius
            .partial_cmp(&a.celsius)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sensors
}

/// Files in `dir` matching `temp*_input`, sorted by their numeric index.
fn files_named(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<(u32, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let index = name
                .strip_prefix(prefix)?
                .strip_suffix(suffix)?
                .parse::<u32>()
                .ok()?;
            Some((index, path))
        })
        .collect();
    paths.sort_by_key(|(index, _)| *index);
    paths.into_iter().map(|(_, path)| path).collect()
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|raw| raw.trim().to_owned())
}

fn read_number(path: &Path) -> Option<f64> {
    let raw = read_trimmed(path)?;
    match raw.parse::<f64>() {
        Ok(value) => Some(value),
        Err(e) => {
            debug!("{} の数値を解釈できません: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batteries_look_sane() {
        for battery in batteries() {
            assert!((0.0..=100.0).contains(&battery.capacity));
        }
    }

    #[test]
    fn sensors_look_sane() {
        for sensor in temperature_sensors() {
            assert!((1.0..150.0).contains(&sensor.celsius));
            assert!(!sensor.chip.is_empty());
        }
    }
}
