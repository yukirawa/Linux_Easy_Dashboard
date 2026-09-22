//! `/sys` readers: batteries.
//!
//! Optional hardware. Every function returns an empty list when the machine has
//! none, and widgets render a quiet "not available" state instead of failing.

use std::fs;
use std::path::Path;

use log::debug;

const POWER_SUPPLY: &str = "/sys/class/power_supply";

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
}
