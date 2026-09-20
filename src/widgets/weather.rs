//! Weather widget, backed by Open-Meteo.
//!
//! * The place name is resolved to coordinates once and cached in the widget's
//!   own settings, so later refreshes are a single request.
//! * Requests run on a worker thread (`gio::spawn_blocking`) and the UI is
//!   updated on the main context, so a slow or offline network never freezes
//!   the dashboard.
//! * Everything degrades to a readable message; nothing here panics or blocks.
//!
//! This is the only widget that talks to the network, and it only does so when
//! the user places it. Open-Meteo needs no API key and stores no account.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyhow::Result;
use gtk::cairo::Context;
use gtk::prelude::*;
use log::debug;
use serde::{Deserialize, Serialize};

use crate::graphics::{self, Bounds, Ink, WeatherGlyph};
use crate::net;
use crate::util::human_percent;

use super::{Category, DetailPage, Spin, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "weather";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "天気",
    summary: "現在の天気と数日間の予報",
    icon: "weather-clear-symbolic",
    category: Category::Info,
    default_size: (400, 260),
    aspect: None,
    build,
    detail,
};

/// How often the forecast is refreshed.
const REFRESH_SECONDS: u32 = 900;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct WeatherConfig {
    /// Free text place name, resolved through Open-Meteo's geocoder.
    location: String,
    /// Resolved coordinates and the canonical place name, cached here so the
    /// geocoder is only asked when the name changes.
    latitude: Option<f64>,
    longitude: Option<f64>,
    place: Option<String>,
    /// Fahrenheit instead of Celsius.
    fahrenheit: bool,
    /// Number of forecast days next to the current conditions.
    days: usize,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            location: "Tokyo".to_owned(),
            latitude: None,
            longitude: None,
            place: None,
            fahrenheit: false,
            days: 4,
        }
    }
}

/// What the widget currently knows, shared with the drawing closure.
#[derive(Default, Clone)]
struct Conditions {
    glyph: Option<WeatherGlyph>,
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: WeatherConfig = context.config();
    let conditions = Rc::new(RefCell::new(Conditions::default()));
    let ink = Ink::new();

    let header = TileHeader::new(DESCRIPTOR.icon, "天気");
    header.set_value(
        &config
            .place
            .clone()
            .unwrap_or_else(|| config.location.clone()),
    );

    let glyph_area = gtk::DrawingArea::new();
    glyph_area.set_content_width(62);
    glyph_area.set_content_height(62);
    glyph_area.set_valign(gtk::Align::Center);
    {
        let conditions = conditions.clone();
        let ink = ink.clone();
        glyph_area.set_draw_func(move |_, cr, width, height| {
            let conditions = conditions.borrow().clone();
            draw_glyph(cr, f64::from(width), f64::from(height), &conditions, &ink);
        });
    }

    let value = gtk::Label::new(None);
    value.add_css_class("edm-readout");
    value.set_xalign(0.0);

    let detail = gtk::Label::new(None);
    detail.add_css_class("caption");
    detail.add_css_class("dim-label");
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    text.append(&value);
    text.append(&detail);

    let current = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    current.set_vexpand(true);
    current.set_valign(gtk::Align::Center);
    current.append(&glyph_area);
    current.append(&text);

    let forecast = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    forecast.set_homogeneous(true);
    forecast.set_halign(gtk::Align::Fill);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&current);
    root.append(&forecast);
    let root = ink.wrap(&root);

    // Weather is fetched on demand: immediately, then every 15 minutes while
    // the tile is on screen.
    let state = Rc::new(State {
        context: context.clone(),
        config,
        conditions,
        ink,
        header,
        value,
        detail,
        forecast,
        glyph_area,
        pending: Cell::new(false),
    });

    let state_for_tick = state.clone();
    super::tick_seconds(&root, REFRESH_SECONDS, move || state_for_tick.refresh());

    Ok(root.upcast())
}

/// The live widget state. Kept in one place so refresh, network callbacks and
/// the drawing closure stay in sync.
struct State {
    context: WidgetContext,
    config: WeatherConfig,
    conditions: Rc<RefCell<Conditions>>,
    ink: Ink,
    header: TileHeader,
    value: gtk::Label,
    detail: gtk::Label,
    forecast: gtk::Box,
    glyph_area: gtk::DrawingArea,
    /// True while a request is in flight, so ticks do not stack up.
    pending: Cell<bool>,
}

impl State {
    /// Fetches the forecast, geocoding the place name first if needed.
    fn refresh(self: &Rc<Self>) {
        if self.pending.get() {
            debug!("天気: 取得中のため待ちます");
            return;
        }
        self.pending.set(true);

        match (self.config.latitude, self.config.longitude) {
            (Some(latitude), Some(longitude)) => self.fetch_forecast(latitude, longitude),
            _ => self.geocode(),
        }
    }

    fn geocode(self: &Rc<Self>) {
        let location = self.config.location.trim().to_owned();
        if location.is_empty() {
            self.fail("場所が設定されていません");
            return;
        }

        let url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=ja&format=json",
            net::encode_query(&location)
        );
        let state = self.clone();
        net::fetch_json(url.clone(), move |result| {
            // Names are resolved as they are typed, so replies can overtake one
            // another. An answer for a name the user has moved on from is dropped.
            if state.current_location() != location {
                debug!("天気: 「{location}」はもう設定されていないため無視します");
                return;
            }
            match result {
                Ok(json) => state.apply_geocode(json),
                Err(message) => {
                    net::report(&url, &message);
                    state.fail(&format!("場所を特定できません: {message}"));
                }
            }
        });
    }

    /// Stores the coordinates, which also triggers the canvas to persist them.
    fn apply_geocode(self: &Rc<Self>, json: serde_json::Value) {
        let Some(first) = json["results"]
            .as_array()
            .and_then(|results| results.first())
        else {
            self.fail(&format!("「{}」が見つかりません", self.config.location));
            return;
        };
        let (Some(latitude), Some(longitude)) =
            (first["latitude"].as_f64(), first["longitude"].as_f64())
        else {
            self.fail("座標を取得できません");
            return;
        };

        let place = first["name"]
            .as_str()
            .unwrap_or(&self.config.location)
            .to_owned();
        // One patch, not three: every patch rebuilds the tile, and each rebuild
        // asks for the forecast again.
        self.context.update(serde_json::json!({
            "latitude": latitude,
            "longitude": longitude,
            "place": place,
        }));
        self.pending.set(false);
        self.fetch_forecast(latitude, longitude);
    }

    fn fetch_forecast(self: &Rc<Self>, latitude: f64, longitude: f64) {
        self.pending.set(true);
        let unit = if self.config.fahrenheit {
            "&temperature_unit=fahrenheit"
        } else {
            ""
        };
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={latitude:.4}&longitude={longitude:.4}\
             &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m\
             &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
             &timezone=auto&forecast_days={}{unit}",
            self.config.days.clamp(1, 7)
        );

        let state = self.clone();
        net::fetch_json(url.clone(), move |result| {
            // Same reasoning as above: a reply that no longer matches the
            // settings it was asked for must not overwrite the newer ones.
            if !state.still_current(latitude, longitude) {
                debug!("天気: 古い設定への応答を無視します");
                return;
            }
            match result {
                Ok(json) => state.apply_forecast(json),
                Err(message) => {
                    net::report(&url, &message);
                    state.fail(&message);
                }
            }
        });
    }

    /// The place name the widget is configured with right now.
    fn current_location(&self) -> String {
        let live: WeatherConfig = self.context.current();
        live.location.trim().to_owned()
    }

    /// Whether the settings a forecast was requested for are still the ones in
    /// force, i.e. whether applying the reply would describe the current widget.
    fn still_current(&self, latitude: f64, longitude: f64) -> bool {
        let live: WeatherConfig = self.context.current();
        live.latitude == Some(latitude)
            && live.longitude == Some(longitude)
            && live.days.clamp(1, 7) == self.config.days.clamp(1, 7)
            && live.fahrenheit == self.config.fahrenheit
    }

    fn apply_forecast(self: &Rc<Self>, json: serde_json::Value) {
        let current = &json["current"];
        let Some(temperature) = current["temperature_2m"].as_f64() else {
            self.fail("応答に気温がありません");
            return;
        };
        let code = current["weather_code"].as_i64().unwrap_or(0);

        *self.conditions.borrow_mut() = Conditions {
            glyph: Some(glyph_for(code)),
        };

        let apparent = current["apparent_temperature"]
            .as_f64()
            .map(|value| format!("体感 {value:.0}°"))
            .unwrap_or_default();
        let humidity = current["relative_humidity_2m"]
            .as_f64()
            .map(|value| format!("湿度 {}", human_percent(value)))
            .unwrap_or_default();
        let wind = current["wind_speed_10m"]
            .as_f64()
            .map(|value| format!("風 {value:.0} km/h"))
            .unwrap_or_default();

        self.value.set_label(&self.format_temperature(temperature));
        self.detail.set_label(&format!(
            "{} · {apparent} · {humidity} · {wind}",
            describe(code)
        ));

        self.update_forecast(&json["daily"]);
        self.glyph_area.queue_draw();
        self.pending.set(false);
    }

    /// One small column per day: weekday, symbol, high/low.
    fn update_forecast(self: &Rc<Self>, daily: &serde_json::Value) {
        while let Some(child) = self.forecast.first_child() {
            self.forecast.remove(&child);
        }

        let (Some(codes), Some(highs), Some(lows), Some(days)) = (
            daily["weather_code"].as_array(),
            daily["temperature_2m_max"].as_array(),
            daily["temperature_2m_min"].as_array(),
            daily["time"].as_array(),
        ) else {
            return;
        };

        for index in 0..days.len().min(highs.len()).min(lows.len()) {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
            column.set_hexpand(true);
            column.set_halign(gtk::Align::Fill);

            let weekday = gtk::Label::new(Some(&weekday_label(days[index].as_str().unwrap_or(""))));
            weekday.add_css_class("caption");
            weekday.add_css_class("dim-label");

            let code = codes
                .get(index)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let conditions = Conditions {
                glyph: Some(glyph_for(code)),
            };
            let symbol = gtk::DrawingArea::new();
            symbol.set_content_width(28);
            symbol.set_content_height(28);
            symbol.set_halign(gtk::Align::Center);
            {
                let ink = self.ink.clone();
                symbol.set_draw_func(move |_, cr, width, height| {
                    draw_glyph(cr, f64::from(width), f64::from(height), &conditions, &ink);
                });
            }

            let range = gtk::Label::new(Some(&format!(
                "{} / {}",
                highs[index]
                    .as_f64()
                    .map_or_else(|| "—".to_owned(), |value| format!("{value:.0}°")),
                lows[index]
                    .as_f64()
                    .map_or_else(|| "—".to_owned(), |value| format!("{value:.0}°"))
            )));
            range.add_css_class("caption");
            range.add_css_class("edm-mono");

            column.append(&weekday);
            column.append(&symbol);
            column.append(&range);
            self.forecast.append(&column);
        }
    }

    fn fail(self: &Rc<Self>, message: &str) {
        *self.conditions.borrow_mut() = Conditions::default();
        self.value.set_label("—");
        self.detail.set_label(message);
        self.header.set_value(
            &self
                .config
                .place
                .clone()
                .unwrap_or_else(|| self.config.location.clone()),
        );
        self.glyph_area.queue_draw();
        self.pending.set(false);
    }

    fn format_temperature(&self, celsius: f64) -> String {
        if self.config.fahrenheit {
            format!("{celsius:.0}°F")
        } else {
            format!("{celsius:.0}°C")
        }
    }
}

fn draw_glyph(cr: &Context, width: f64, height: f64, conditions: &Conditions, ink: &Ink) {
    graphics::prepare(cr);
    let Some(glyph) = conditions.glyph else {
        return;
    };
    graphics::weather_glyph(
        cr,
        Bounds::new(0.0, 0.0, width, height),
        glyph,
        &ink.fg(),
        &ink.accent(),
    );
}

/// WMO weather codes, as used by Open-Meteo.
fn glyph_for(code: i64) -> WeatherGlyph {
    match code {
        0 => WeatherGlyph::Clear,
        1 | 2 => WeatherGlyph::PartlyCloudy,
        3 => WeatherGlyph::Cloudy,
        45 | 48 => WeatherGlyph::Fog,
        51 | 53 | 55 | 56 | 57 => WeatherGlyph::Drizzle,
        61 | 63 | 65 | 66 | 67 | 80 | 81 | 82 => WeatherGlyph::Rain,
        71 | 73 | 75 | 77 | 85 | 86 => WeatherGlyph::Snow,
        95 | 96 | 99 => WeatherGlyph::Thunder,
        _ => WeatherGlyph::Cloudy,
    }
}

fn describe(code: i64) -> &'static str {
    match code {
        0 => "快晴",
        1 => "晴れ",
        2 => "晴れ時々曇り",
        3 => "曇り",
        45 | 48 => "霧",
        51 | 53 | 55 => "霧雨",
        56 | 57 => "着氷性の霧雨",
        61 => "小雨",
        63 => "雨",
        65 => "強い雨",
        66 | 67 => "着氷性の雨",
        71 => "小雪",
        73 => "雪",
        75 => "大雪",
        77 => "霧雪",
        80 => "にわか雨",
        81 => "強いにわか雨",
        82 => "激しいにわか雨",
        85 | 86 => "にわか雪",
        95 => "雷雨",
        96 | 99 => "雷雨（ひょう）",
        _ => "不明",
    }
}

/// `2026-09-20` -> `(日)` style weekday, localized.
fn weekday_label(date: &str) -> String {
    let Ok(datetime) = glib::DateTime::from_iso8601(&format!("{date}T12:00:00"), None) else {
        return date.to_owned();
    };
    datetime
        .format("%a")
        .map_or_else(|_| date.to_owned(), |text| text.to_string())
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: WeatherConfig = context.config();
    let page = DetailPage::new("現在の天気", "設定");

    match &config.place {
        Some(place) => page.fact("場所", place),
        None => page.note("場所を入力すると座標を調べて天気を取得します。"),
    }
    if let (Some(latitude), Some(longitude)) = (config.latitude, config.longitude) {
        page.fact("座標", &format!("{latitude:.3}, {longitude:.3}"));
    }

    // Changing the name invalidates the cached coordinates, otherwise the tile
    // would keep showing the weather of the place it already resolved.
    page.text(context, "場所 (都市名)", &config.location, |text| {
        serde_json::json!({
            "location": text.trim(),
            "latitude": null,
            "longitude": null,
            "place": null,
        })
    });
    page.spin(
        context,
        Spin::new("days", "予報の日数", config.days as f64, 1.0, 7.0),
    );
    page.switch(context, "fahrenheit", "華氏で表示", config.fahrenheit);
    page.note("天気は Open-Meteo (open-meteo.com) から取得します。API キーは不要です。");
    page.note("場所を書き換えると、座標を調べ直して天気を取り直します。");

    Ok(page.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_codes_map_to_glyphs() {
        assert_eq!(glyph_for(0), WeatherGlyph::Clear);
        assert_eq!(glyph_for(3), WeatherGlyph::Cloudy);
        assert_eq!(glyph_for(63), WeatherGlyph::Rain);
        assert_eq!(glyph_for(75), WeatherGlyph::Snow);
        assert_eq!(glyph_for(95), WeatherGlyph::Thunder);
        assert_eq!(glyph_for(1234), WeatherGlyph::Cloudy);
    }

    #[test]
    fn descriptions_are_never_empty() {
        for code in 0..=99 {
            assert!(!describe(code).is_empty());
        }
    }
}
