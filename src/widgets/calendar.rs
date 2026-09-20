//! Calendar widget: the current month, with today highlighted.
//!
//! Weekday and month names come from the locale through `glib::DateTime`, so a
//! Japanese or English session both read correctly.

use anyhow::Result;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::util::is_leap_year;

use super::{Category, DetailPage, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "calendar";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "カレンダー",
    summary: "今月のカレンダー",
    icon: "x-office-calendar-symbolic",
    category: Category::Time,
    default_size: (360, 280),
    aspect: None,
    build,
    detail,
};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
struct CalendarConfig {
    /// Start the week on Monday instead of Sunday.
    week_starts_monday: bool,
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: CalendarConfig = context.config();

    let header = gtk::Label::new(None);
    header.add_css_class("heading");
    header.set_xalign(0.0);
    header.set_hexpand(true);

    let weekdays = gtk::Label::new(None);
    weekdays.add_css_class("caption");
    weekdays.add_css_class("dim-label");
    weekdays.set_xalign(1.0);

    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    title_row.append(&header);
    title_row.append(&weekdays);

    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .row_homogeneous(true)
        .row_spacing(2)
        .column_spacing(2)
        .build();
    grid.set_vexpand(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_vexpand(true);
    root.append(&title_row);
    root.append(&grid);

    let view = View {
        header,
        weekdays,
        grid,
        config,
    };

    super::tick_seconds(&root, 30, move || view.refresh());
    Ok(root.upcast())
}

struct View {
    header: gtk::Label,
    weekdays: gtk::Label,
    grid: gtk::Grid,
    config: CalendarConfig,
}

impl View {
    fn refresh(&self) {
        let Ok(today) = glib::DateTime::now_local() else {
            return;
        };
        let (year, month) = (today.year(), today.month());

        self.header.set_label(
            &today
                .format("%Y年 %B")
                .unwrap_or_else(|_| glib::GString::from(format!("{year}-{month:02}"))),
        );

        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }

        let columns: i32 = 7;
        let offset = self.week_offset(year, month).unwrap_or(0);
        let days = days_in_month(year, month);
        let today_day = today.day_of_month();

        self.weekdays
            .set_label(&weekday_names(self.config.week_starts_monday).join("  "));

        // Day cells: blanks before the first of the month.
        for cell in 0..(offset + days) {
            let column = cell % columns;
            let row = cell / columns;
            let label = gtk::Label::new(None);
            label.add_css_class("edm-day-cell");
            if cell < offset {
                self.grid.attach(&label, column, row, 1, 1);
                continue;
            }
            let day = cell - offset + 1;
            label.set_label(&day.to_string());
            if day == today_day {
                label.add_css_class("edm-today");
            }
            self.grid.attach(&label, column, row, 1, 1);
        }
    }

    /// Blank cells before the first of the month.
    fn week_offset(&self, year: i32, month: i32) -> Option<i32> {
        let first =
            glib::DateTime::new(&glib::TimeZone::local(), year, month, 1, 0, 0, 0.0).ok()?;
        // GLib counts Monday as 1, so shift for a Sunday-first week.
        let weekday = first.day_of_week();
        Some(if self.config.week_starts_monday {
            weekday - 1
        } else {
            weekday % 7
        })
    }
}

/// Days in a month, respecting the Gregorian leap rule.
fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 30,
    }
}

/// Localized short weekday names, in the order the grid displays them.
///
/// 2024-01-01 was a Monday, so that week provides one day per weekday without
/// having to know which date is being shown.
fn weekday_names(week_starts_monday: bool) -> Vec<String> {
    let timezone = glib::TimeZone::local();
    let start = if week_starts_monday { 1 } else { 7 };
    (0..7)
        .filter_map(|offset| {
            let weekday = (start + offset - 1) % 7 + 1;
            glib::DateTime::new(&timezone, 2024, 1, weekday, 0, 0, 0.0)
                .ok()?
                .format("%a")
                .ok()
                .map(|text| text.to_string())
        })
        .collect()
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: CalendarConfig = context.config();
    let page = DetailPage::new("カレンダー", "表示");
    page.switch(
        context,
        "week_starts_monday",
        "月曜日から始める",
        config.week_starts_monday,
    );
    Ok(page.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_lengths_follow_the_gregorian_rule() {
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
    }
}
