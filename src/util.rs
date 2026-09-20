//! Small helpers shared by widgets: display formatting and a history buffer.

use std::collections::VecDeque;
use std::time::Duration;

/// Formats a byte count using binary units, e.g. `1.4 GiB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formats a duration as uptime text, e.g. `3日 04:05` or `04:05`.
pub fn human_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    if days > 0 {
        format!("{days}日 {hours:02}:{minutes:02}")
    } else {
        format!("{hours:02}:{minutes:02}")
    }
}

/// Whether `year` has 366 days, by the Gregorian rule.
pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// 366 in a leap year, 365 otherwise.
pub fn days_in_year(year: i32) -> i32 {
    if is_leap_year(year) { 366 } else { 365 }
}

/// Formats a `0.0..=100.0` value for display.
pub fn human_percent(value: f64) -> String {
    format!("{value:.0}%")
}

/// Formats a rate in bytes per second, e.g. `1.2 MiB/s`.
pub fn human_rate(bytes_per_second: f64) -> String {
    let bytes = bytes_per_second.max(0.0);
    let mut value = bytes;
    let mut unit = 0;
    const UNITS: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A fixed size window of recent values, oldest first.
///
/// Sparklines and gauges keep one of these per metric; pushing beyond the
/// capacity drops the oldest sample, so memory is constant.
#[derive(Debug, Clone)]
pub struct History {
    values: VecDeque<f64>,
    capacity: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(capacity.max(2)),
            capacity: capacity.max(2),
        }
    }

    pub fn push(&mut self, value: f64) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    pub fn latest(&self) -> Option<f64> {
        self.values.back().copied()
    }

    pub fn maximum(&self) -> f64 {
        self.values.iter().copied().fold(0.0, f64::max)
    }

    pub fn average(&self) -> f64 {
        if self.values.is_empty() {
            0.0
        } else {
            self.values.iter().sum::<f64>() / self.values.len() as f64
        }
    }

    /// Oldest first, for drawing.
    pub fn samples(&self) -> Vec<f64> {
        self.values.iter().copied().collect()
    }

    /// Repeats the newest sample until there are at least `count` of them, so a
    /// freshly added widget draws a flat line instead of a gap.
    pub fn seed(&mut self, count: usize) {
        if let Some(latest) = self.latest() {
            while self.values.len() < count.min(self.capacity) {
                self.push(latest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(human_bytes(600 * 1024 * 1024), "600 MiB");
    }

    #[test]
    fn durations_read_like_uptime() {
        assert_eq!(human_duration(Duration::from_secs(3_900)), "01:05");
        assert_eq!(human_duration(Duration::from_secs(90_000)), "1日 01:00");
    }

    #[test]
    fn leap_years_follow_the_gregorian_rule() {
        assert!(is_leap_year(2024));
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(!is_leap_year(2026));
        assert_eq!(days_in_year(2024), 366);
        assert_eq!(days_in_year(2026), 365);
    }

    #[test]
    fn rates_and_history() {
        assert_eq!(human_rate(512.0), "512 B/s");
        assert_eq!(human_rate(1536.0), "1.5 KiB/s");

        let mut history = History::new(3);
        for value in [1.0, 2.0, 3.0, 4.0] {
            history.push(value);
        }
        assert_eq!(history.samples(), vec![2.0, 3.0, 4.0]);
        assert_eq!(history.latest(), Some(4.0));
        assert_eq!(history.maximum(), 4.0);
        assert_eq!(history.average(), 3.0);

        let mut fresh = History::new(5);
        fresh.push(7.0);
        fresh.seed(4);
        assert_eq!(fresh.samples().len(), 4);
        assert_eq!(fresh.latest(), Some(7.0));
    }
}
