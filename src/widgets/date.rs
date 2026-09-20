//! Date widget: today at a glance — the day set large, the year's progress
//! underneath.
//!
//! Weekday and month names come from the locale through `glib::DateTime`, and
//! the week number is the ISO 8601 one, so it matches calendars and logs.

use anyhow::Result;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::util::{days_in_year, human_percent, is_leap_year};

use super::{Category, DetailPage, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "date";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "今日",
    summary: "日付と年の進み具合",
    icon: "document-open-recent-symbolic",
    category: Category::Time,
    default_size: (320, 220),
    aspect: None,
    build,
    detail,
};

/// A date changes at midnight; this is only here to catch that.
const INTERVAL: u32 = 30;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct DateConfig {
    /// Show the ISO week number in the header.
    show_week: bool,
    /// Show how much of the year has passed.
    show_progress: bool,
}

impl Default for DateConfig {
    fn default() -> Self {
        Self {
            show_week: true,
            show_progress: true,
        }
    }
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: DateConfig = context.config();

    let header = TileHeader::new(DESCRIPTOR.icon, "今日");

    let day = gtk::Label::new(None);
    day.add_css_class("edm-date-day");
    day.set_xalign(0.0);
    day.set_valign(gtk::Align::Center);

    let weekday = gtk::Label::new(None);
    weekday.add_css_class("edm-date-weekday");
    weekday.set_xalign(0.0);

    let month = gtk::Label::new(None);
    month.add_css_class("caption");
    month.add_css_class("dim-label");
    month.set_xalign(0.0);

    let side = gtk::Box::new(gtk::Orientation::Vertical, 2);
    side.set_valign(gtk::Align::Center);
    side.set_hexpand(true);
    side.append(&weekday);
    side.append(&month);

    let big = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    big.set_vexpand(true);
    big.append(&day);
    big.append(&side);

    let progress = gtk::Label::new(None);
    progress.add_css_class("caption");
    progress.add_css_class("dim-label");
    progress.set_xalign(0.0);
    progress.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let bar = gtk::ProgressBar::new();
    bar.add_css_class("edm-bar");
    bar.set_show_text(false);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&big);
    if config.show_progress {
        root.append(&progress);
        root.append(&bar);
    }

    super::tick_seconds(&root, INTERVAL, move || {
        let Ok(today) = glib::DateTime::now_local() else {
            return;
        };
        day.set_label(&today.day_of_month().to_string());
        weekday.set_label(&glib_text(&today, "%A"));
        month.set_label(&glib_text(&today, "%Y年 %B"));

        let week = if config.show_week {
            format!("第 {} 週", today.week_of_year())
        } else {
            String::new()
        };
        header.set_value(&week);

        if config.show_progress {
            let fraction = year_fraction(today.year(), today.day_of_year());
            bar.set_fraction(fraction);
            let remaining = days_in_year(today.year()) - today.day_of_year();
            progress.set_label(&format!(
                "{}年の {} が経過 · 残り {remaining} 日",
                today.year(),
                human_percent(fraction * 100.0)
            ));
        }
    });

    Ok(root.upcast())
}

/// Localized text, or an empty string when the format is not supported.
fn glib_text(datetime: &glib::DateTime, format: &str) -> String {
    datetime
        .format(format)
        .map_or_else(|_| String::new(), |text| text.to_string())
}

/// How far through the year a 1-based day number is.
fn year_fraction(year: i32, day_of_year: i32) -> f64 {
    let days = f64::from(days_in_year(year).max(1));
    (f64::from(day_of_year) / days).clamp(0.0, 1.0)
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: DateConfig = context.config();
    let page = DetailPage::new("今日", "表示");

    let Ok(today) = glib::DateTime::now_local() else {
        page.note("現在時刻を取得できませんでした。");
        return Ok(page.finish());
    };

    let (year, day_of_year) = (today.year(), today.day_of_year());
    let total = days_in_year(year);
    let fraction = year_fraction(year, day_of_year);

    page.fact("日付", &glib_text(&today, "%Y年%m月%d日"));
    page.fact("曜日", &glib_text(&today, "%A"));
    page.fact("週", &format!("第 {} 週 (ISO 8601)", today.week_of_year()));
    page.fact("通算", &format!("{day_of_year} 日目 / {total} 日"));
    page.fact("残り", &format!("{} 日", total - day_of_year));
    page.fact("年の経過", &human_percent(fraction * 100.0));
    page.fact(
        "うるう年",
        if is_leap_year(year) {
            "はい"
        } else {
            "いいえ"
        },
    );

    page.switch(context, "show_week", "週番号を表示", config.show_week);
    page.switch(
        context,
        "show_progress",
        "年の進み具合を表示",
        config.show_progress,
    );
    Ok(page.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_year_is_over_when_the_last_day_is() {
        assert_eq!(year_fraction(2026, 1), 1.0 / 365.0);
        assert_eq!(year_fraction(2026, 365), 1.0);
        assert_eq!(year_fraction(2024, 366), 1.0);
        assert_eq!(year_fraction(2024, 1), 1.0 / 366.0);
    }

    #[test]
    fn fractions_stay_inside_the_bar() {
        assert_eq!(year_fraction(2026, 0), 0.0);
        assert_eq!(year_fraction(2026, 400), 1.0);
    }
}
