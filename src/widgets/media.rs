//! Media player widget, driven by MPRIS over the session bus.
//!
//! * Players are found by listing the session bus and keeping the names under
//!   `org.mpris.MediaPlayer2.`, so any MPRIS capable player works — Spotify,
//!   Firefox, mpv, VLC.
//! * Every call is asynchronous, so a player that hangs or quits cannot freeze
//!   the dashboard. Errors are swallowed and read as "nothing is playing".
//! * `Position` is a snapshot taken when the properties arrive, so the bar is
//!   advanced locally between the once-a-second polls.
//!
//! Nothing here has to be configured: with an empty player name the tile
//! follows whichever player is playing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use anyhow::Result;
use glib::Variant;
use gtk::prelude::*;
use gtk::{gio, glib};
use libadwaita::prelude::*;
use log::debug;
use serde::{Deserialize, Serialize};

use super::{Category, DetailPage, TileHeader, WidgetContext, WidgetDescriptor};

pub const KIND: &str = "media";

pub const DESCRIPTOR: WidgetDescriptor = WidgetDescriptor {
    kind: KIND,
    name: "メディアプレイヤー",
    summary: "再生中のトラックと操作",
    icon: "audio-x-generic-symbolic",
    category: Category::Info,
    default_size: (360, 220),
    aspect: None,
    build,
    detail,
};

/// Every MPRIS player publishes its bus name under this prefix.
const PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// The well known path and interfaces of a player.
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
/// The session bus daemon itself, used to enumerate the names on the bus.
const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const BUS_INTERFACE: &str = "org.freedesktop.DBus";
/// A player that has hung must not hold on to its reply forever.
const TIMEOUT_MS: i32 = 2000;
/// How often the tile is polled while it is on screen.
const TICK_SECONDS: u32 = 1;
/// Idle sweeps are this many ticks apart, so an empty tile is nearly free.
const LIST_EVERY: u32 = 5;
/// Players are queried in parallel; more than this many is not worth it.
const MAX_PLAYERS: usize = 8;
/// Artwork size in the tile.
const COVER_SIZE: i32 = 56;
/// Shown while the detail view is still waiting for the bus.
const PENDING: &str = "取得中…";
/// Shown in place of a missing value.
const DASH: &str = "—";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct MediaConfig {
    /// Bus name suffix of the player to follow. Empty follows whatever plays.
    player: String,
    /// Hide the tile when nothing is playing.
    only_while_playing: bool,
    show_controls: bool,
    show_art: bool,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            player: String::new(),
            only_while_playing: true,
            show_controls: true,
            show_art: true,
        }
    }
}

/// MPRIS `PlaybackStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Playing,
    Paused,
    Stopped,
}

impl State {
    /// How worth showing this state is, used to choose between several players.
    fn rank(self) -> u8 {
        match self {
            Self::Playing => 2,
            Self::Paused => 1,
            Self::Stopped => 0,
        }
    }
}

/// One snapshot of a player's properties, taken together so the tile never
/// mixes an old title with a new position.
#[derive(Debug, Clone)]
struct Player {
    identity: String,
    state: State,
    title: String,
    artist: String,
    album: String,
    /// `mpris:length`, in microseconds. Zero when the player does not say.
    length: i64,
    art_url: String,
    /// `Position` at the time of the snapshot, in microseconds.
    position: i64,
    /// When `position` was read, so the bar can be advanced locally.
    at: Instant,
}

/// The reply of a single D-Bus call.
type Reply = std::result::Result<Variant, glib::Error>;

/// Runs exactly once, once a probe has an answer (or gives up).
type OnPlayer = Rc<dyn Fn(Option<(String, Player)>)>;

/// A thin, panic free wrapper over the D-Bus calls this widget makes. Nothing
/// here blocks: every call hands its answer to a callback on the main context.
#[derive(Clone)]
struct Bus {
    connection: gio::DBusConnection,
}

impl Bus {
    /// The session bus, or `None` when there is none to talk to.
    fn session() -> Option<Self> {
        match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(connection) => Some(Self { connection }),
            Err(e) => {
                debug!("メディア: セッションバスに接続できません: {e}");
                None
            }
        }
    }

    /// One asynchronous call on a player object. Errors are logged and handed
    /// on, so a player that hangs or quits reads as "nothing is playing" rather
    /// than as a failure.
    ///
    /// The reply type is deliberately left open: a method's answer is wrapped in
    /// its return value (`ListNames` answers `(as)`, `GetAll` answers `(a{sv})`),
    /// and gio rejects a declared type that does not match the message exactly.
    /// The callers unwrap with [`payload`] instead.
    fn call(
        &self,
        bus_name: Option<&str>,
        interface: &str,
        method: &str,
        parameters: Option<&Variant>,
        on_done: impl FnOnce(Reply) + 'static,
    ) {
        let label = format!("{interface}.{method}");
        self.connection.call(
            bus_name,
            PLAYER_PATH,
            interface,
            method,
            parameters,
            None,
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
            gio::Cancellable::NONE,
            move |result| {
                report(&label, &result);
                on_done(result);
            },
        );
    }

    /// Every name on the session bus, or `None` when the daemon did not answer
    /// with a list of strings. The daemon lives at its own object path, so this
    /// one call does not go through [`Bus::call`].
    fn list_names(&self, on_done: impl FnOnce(Option<Vec<String>>) + 'static) {
        self.connection.call(
            Some(BUS_NAME),
            BUS_PATH,
            BUS_INTERFACE,
            "ListNames",
            None,
            None,
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
            gio::Cancellable::NONE,
            move |result| {
                report("org.freedesktop.DBus.ListNames", &result);
                on_done(
                    result
                        .ok()
                        .map(payload)
                        .and_then(|names| names.get::<Vec<String>>()),
                );
            },
        );
    }

    /// Every property of one interface, as an `a{sv}` dictionary.
    fn get_all(&self, bus_name: &str, interface: &str, on_done: impl FnOnce(Reply) + 'static) {
        let parameters = (interface,).to_variant();
        self.call(
            Some(bus_name),
            PROPERTIES_INTERFACE,
            "GetAll",
            Some(&parameters),
            move |reply| on_done(reply.map(payload)),
        );
    }

    /// `Position`, in microseconds. Players are free to omit it.
    fn get_position(&self, bus_name: &str, on_done: impl FnOnce(Option<i64>) + 'static) {
        let parameters = (PLAYER_INTERFACE, "Position").to_variant();
        self.call(
            Some(bus_name),
            PROPERTIES_INTERFACE,
            "Get",
            Some(&parameters),
            move |reply| {
                let position = reply.ok().and_then(|reply| {
                    let value = unbox(&payload(reply));
                    variant_i64(&value)
                });
                on_done(position);
            },
        );
    }

    /// Sends a transport command. The reply carries nothing worth reading.
    fn command(&self, bus_name: &str, method: &str) {
        self.call(Some(bus_name), PLAYER_INTERFACE, method, None, |_| {});
    }
}

/// The single value a method returned.
///
/// D-Bus wraps a method's result in its return value, so `ListNames` hands back
/// `(as)` and `GetAll` hands back `(a{sv})` — a one element tuple. Reading that
/// tuple's only child is what turns a reply into the value inside it. Anything
/// that is not a one element tuple (an empty reply, or a dictionary that happens
/// to have one entry) is handed back untouched.
fn payload(reply: Variant) -> Variant {
    let is_tuple = reply.type_().as_str().starts_with('(');
    if is_tuple && reply.n_children() == 1 {
        reply.child_value(0)
    } else {
        reply
    }
}

/// A failed call is normal now and then — a player quit, a property is missing —
/// so it is logged rather than shown.
fn report(label: &str, result: &Reply) {
    if let Err(e) = result {
        debug!("メディア: {label} が失敗しました: {e}");
    }
}

/// Strips the MPRIS prefix: `org.mpris.MediaPlayer2.spotify` -> `spotify`.
fn bus_suffix(name: &str) -> &str {
    name.strip_prefix(PLAYER_PREFIX).unwrap_or(name)
}

/// Whether a bus name belongs to an MPRIS player (and is not the bare prefix).
fn is_player_name(name: &str) -> bool {
    name.len() > PLAYER_PREFIX.len() && name.starts_with(PLAYER_PREFIX)
}

/// Whether this player is the one the settings ask for. An empty name means
/// "any", and otherwise the bus name suffix is matched as a prefix, so
/// `spotify` finds `org.mpris.MediaPlayer2.spotify`.
fn matches_player(suffix: &str, wanted: &str) -> bool {
    let wanted = wanted.trim();
    wanted.is_empty() || suffix.to_lowercase().starts_with(&wanted.to_lowercase())
}

/// Maps `PlaybackStatus` onto the three states the widget knows.
fn playback_state(status: &str) -> State {
    if status.eq_ignore_ascii_case("Playing") {
        State::Playing
    } else if status.eq_ignore_ascii_case("Paused") {
        State::Paused
    } else {
        State::Stopped
    }
}

/// `184_000_000` -> `3:04`. Values players fail to report read as unknown.
fn format_time(micros: i64) -> String {
    if micros <= 0 {
        return "--:--".to_owned();
    }
    let seconds = micros / 1_000_000;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// How full the bar is, clamped so a wrong or missing length cannot overflow it.
fn progress_fraction(position: i64, length: i64) -> f64 {
    if length <= 0 {
        return 0.0;
    }
    (position.max(0) as f64 / length as f64).clamp(0.0, 1.0)
}

/// `mpris:artUrl` is only usable as a file when it points at one.
fn is_local_art(url: &str) -> bool {
    url.starts_with("file://")
}

/// The path behind a `file://` artwork URL, decoded.
fn local_art_path(url: &str) -> Option<PathBuf> {
    if !is_local_art(url) {
        return None;
    }
    glib::filename_from_uri(url).ok().map(|(path, _host)| path)
}

/// Reads a child of an `a{sv}` dictionary, unboxing the variant for the caller.
fn dict_value(dict: &Variant, key: &str) -> Option<Variant> {
    dict.get::<HashMap<String, Variant>>()?.remove(key)
}

/// A string member of an `a{sv}` dictionary: `PlaybackStatus`, `xesam:title`,
/// `Identity` and friends all read through this. Missing entries, wrong types
/// and blank strings all come back as `None`.
fn metadata_text(dict: &Variant, key: &str) -> Option<String> {
    dict_value(dict, key)?
        .get::<String>()
        .filter(|text| !text.trim().is_empty())
}

/// A list of strings, as `xesam:artist` is specified.
fn metadata_list(dict: &Variant, key: &str) -> Option<Vec<String>> {
    dict_value(dict, key)?
        .get::<Vec<String>>()
        .filter(|values| !values.is_empty())
}

/// A number, as `mpris:length` is specified. Players disagree on the width, so
/// every signed and unsigned integer type is accepted.
fn metadata_int(dict: &Variant, key: &str) -> Option<i64> {
    variant_i64(&dict_value(dict, key)?)
}

fn variant_i64(value: &Variant) -> Option<i64> {
    if let Some(number) = value.get::<i64>() {
        return Some(number);
    }
    if let Some(number) = value.get::<u64>() {
        return i64::try_from(number).ok();
    }
    if let Some(number) = value.get::<i32>() {
        return Some(i64::from(number));
    }
    if let Some(number) = value.get::<u32>() {
        return Some(i64::from(number));
    }
    if let Some(number) = value.get::<i16>() {
        return Some(i64::from(number));
    }
    if let Some(number) = value.get::<u16>() {
        return Some(i64::from(number));
    }
    None
}

/// `Properties.Get` boxes its answer, unlike `GetAll`.
fn unbox(value: &Variant) -> Variant {
    value.as_variant().unwrap_or_else(|| value.clone())
}

/// A never-empty label, so a player without tags does not leave a blank line.
fn label_or_dash(text: &str) -> &str {
    if text.trim().is_empty() { DASH } else { text }
}

/// The artist and album line under the title.
fn track_line(artist: &str, album: &str) -> String {
    match (artist.trim(), album.trim()) {
        ("", "") => DASH.to_owned(),
        (artist, "") => artist.to_owned(),
        ("", album) => album.to_owned(),
        (artist, album) => format!("{artist} · {album}"),
    }
}

/// Turns one player's `org.mpris.MediaPlayer2.Player` properties into a
/// snapshot. Anything unexpected is simply left out; nothing here panics.
fn parse_player(bus_name: &str, properties: &Variant) -> Player {
    let state = metadata_text(properties, "PlaybackStatus")
        .map_or(State::Stopped, |status| playback_state(&status));

    let metadata = dict_value(properties, "Metadata");
    let (title, artist, album, length, art_url) = match &metadata {
        Some(metadata) => (
            metadata_text(metadata, "xesam:title").unwrap_or_default(),
            metadata_list(metadata, "xesam:artist")
                .map(|artists| artists.join(", "))
                .unwrap_or_default(),
            metadata_text(metadata, "xesam:album").unwrap_or_default(),
            metadata_int(metadata, "mpris:length").unwrap_or(0),
            metadata_text(metadata, "mpris:artUrl").unwrap_or_default(),
        ),
        None => (
            String::new(),
            String::new(),
            String::new(),
            0,
            String::new(),
        ),
    };

    Player {
        identity: bus_suffix(bus_name).to_owned(),
        state,
        title,
        artist,
        album,
        length,
        art_url,
        position: 0,
        at: Instant::now(),
    }
}

/// Queries every candidate in parallel, keeps the one most worth showing, then
/// fills in its display name and playback position. The callback runs exactly
/// once, whatever the players answer.
fn probe_best(bus: &Bus, candidates: Vec<String>, on_done: OnPlayer) {
    if candidates.is_empty() {
        on_done(None);
        return;
    }

    let found: Rc<RefCell<Vec<(String, Player)>>> = Rc::new(RefCell::new(Vec::new()));
    let remaining = Rc::new(Cell::new(candidates.len()));

    for name in candidates {
        // Two handles: one borrows for the call, one moves into the reply.
        let caller = bus.clone();
        let player_bus = bus.clone();
        let key = name.clone();
        let found = found.clone();
        let remaining = remaining.clone();
        let on_done = on_done.clone();

        caller.get_all(&key, PLAYER_INTERFACE, move |reply| {
            if let Ok(properties) = reply {
                found
                    .borrow_mut()
                    .push((name.clone(), parse_player(&name, &properties)));
            }

            let left = remaining.get().saturating_sub(1);
            remaining.set(left);
            if left > 0 {
                return;
            }

            let best = found
                .borrow_mut()
                .drain(..)
                .max_by_key(|(_, player)| player.state.rank());
            match best {
                Some((name, player)) => finish_player(&player_bus, name, player, on_done),
                None => on_done(None),
            }
        });
    }
}

/// Reads the two properties that are not part of the player interface's own
/// `GetAll`, then hands over the finished snapshot.
fn finish_player(bus: &Bus, name: String, mut player: Player, on_done: OnPlayer) {
    let caller = bus.clone();
    let details = bus.clone();
    let key = name.clone();

    caller.get_all(&key, ROOT_INTERFACE, move |reply| {
        player.identity = reply
            .ok()
            .and_then(|properties| metadata_text(&properties, "Identity"))
            .unwrap_or_else(|| bus_suffix(&name).to_owned());

        let caller = details.clone();
        let key = name.clone();
        caller.get_position(&key, move |position| {
            if let Some(position) = position {
                player.position = position;
            }
            player.at = Instant::now();
            on_done(Some((name, player)));
        });
    });
}

/// The tile's live state, kept in one place so the tick, the callbacks and the
/// buttons stay in sync.
struct Live {
    context: WidgetContext,
    config: MediaConfig,
    header: TileHeader,
    cover: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    body: gtk::Box,
    bar: gtk::ProgressBar,
    controls: gtk::Box,
    play: gtk::Button,
    empty: gtk::Label,
    /// The player being followed, if one is playing or paused.
    player: RefCell<Option<String>>,
    /// The session bus, cached once it has been fetched.
    connection: RefCell<Option<gio::DBusConnection>>,
    ticks: Cell<u32>,
    /// True while the calls of one refresh are in flight, so ticks cannot stack.
    pending: Cell<bool>,
}

impl Live {
    /// The session bus, fetched at most once.
    fn bus(&self) -> Option<Bus> {
        let cached = self.connection.borrow().clone();
        if let Some(connection) = cached {
            return Some(Bus { connection });
        }
        let bus = Bus::session()?;
        *self.connection.borrow_mut() = Some(bus.connection.clone());
        Some(bus)
    }

    /// One tick: follow the known player, and sweep the bus for new ones every
    /// few seconds so a player started later is picked up.
    fn refresh(self: &Rc<Self>) {
        if self.pending.get() {
            return;
        }
        self.pending.set(true);

        let tick = self.ticks.get().wrapping_add(1);
        self.ticks.set(tick);

        let known = self.player.borrow().clone();
        match known {
            Some(name) if tick % LIST_EVERY != 1 => self.probe(name),
            _ if tick % LIST_EVERY == 1 => self.discover(),
            _ => self.pending.set(false),
        }
    }

    /// Asks one known player for its properties.
    fn probe(self: &Rc<Self>, name: String) {
        let Some(bus) = self.bus() else {
            self.forget();
            return;
        };
        let live = self.clone();
        probe_best(
            &bus,
            vec![name],
            Rc::new(move |result| match result {
                Some((name, player)) => live.show(name, player),
                None => live.forget(),
            }),
        );
    }

    /// Lists the bus, then asks every player that matches the settings.
    fn discover(self: &Rc<Self>) {
        let Some(bus) = self.bus() else {
            self.forget();
            return;
        };
        let wanted = self.config.player.clone();
        let live = self.clone();
        let handle = bus.clone();

        handle.list_names(move |names| {
            let Some(names) = names else {
                live.forget();
                return;
            };
            let candidates: Vec<String> = names
                .into_iter()
                .filter(|name| is_player_name(name) && matches_player(bus_suffix(name), &wanted))
                .take(MAX_PLAYERS)
                .collect();

            let live = live.clone();
            let on_done: OnPlayer = Rc::new(move |result| match result {
                Some((name, player)) => live.show(name, player),
                None => live.forget(),
            });
            probe_best(&bus, candidates, on_done);
        });
    }

    /// The bus answered: show the player, or the empty message when it has
    /// nothing playing.
    fn show(&self, name: String, player: Player) {
        self.pending.set(false);
        if player.state == State::Stopped {
            // Nothing to follow: dropping the name means the tile goes back to
            // sweeping the bus every few seconds instead of polling this player
            // once a second.
            *self.player.borrow_mut() = None;
            self.show_none();
            return;
        }

        self.header.set_value(&player.identity);
        self.title.set_label(label_or_dash(&player.title));
        self.subtitle
            .set_label(&track_line(&player.artist, &player.album));
        self.set_cover(&player.art_url);
        self.bar.set_fraction(progress_now(&player));
        self.play.set_icon_name(match player.state {
            State::Playing => "media-playback-pause-symbolic",
            _ => "media-playback-start-symbolic",
        });

        self.body.set_visible(true);
        self.bar.set_visible(true);
        self.controls.set_visible(self.config.show_controls);
        self.empty.set_visible(false);
        self.context.set_hidden(false);
        {
            // Worth a line: "the media widget shows nothing" is the usual report,
            // and this says which player was chosen and what it reported.
            let playing = progress_now(&player);
            debug!(
                "メディア: {} [{}] 「{}」 {} ({}/{} の {:.0}%) を表示します",
                player.identity,
                match player.state {
                    State::Playing => "Playing",
                    State::Paused => "Paused",
                    State::Stopped => "Stopped",
                },
                player.title,
                player.artist,
                format_time(player.position),
                format_time(player.length),
                playing * 100.0,
            );
        }
        *self.player.borrow_mut() = Some(name);
    }

    /// Nothing is playing: say so, and let the tile hide itself when asked to.
    fn show_none(&self) {
        self.pending.set(false);
        self.header.set_value("");
        self.title.set_label("");
        self.subtitle.set_label("");
        self.bar.set_fraction(0.0);
        self.body.set_visible(false);
        self.bar.set_visible(false);
        self.controls.set_visible(false);
        self.empty.set_visible(true);
        self.context.set_hidden(self.config.only_while_playing);
    }

    /// The player that was being followed is gone; go back to sweeping.
    fn forget(&self) {
        *self.player.borrow_mut() = None;
        self.show_none();
    }

    /// Sends a transport command to the player being shown.
    fn command(&self, method: &str) {
        let known = self.player.borrow().clone();
        let Some(name) = known else {
            return;
        };
        let Some(bus) = self.bus() else {
            return;
        };
        bus.command(&name, method);
    }

    /// Puts the artwork up when the player offers a local file, and the widget's
    /// own icon otherwise (web URLs are never fetched).
    fn set_cover(&self, art_url: &str) {
        let path = if self.config.show_art {
            local_art_path(art_url)
        } else {
            None
        };
        match path {
            Some(path) => self.cover.set_from_file(Some(path)),
            None => self.cover.set_icon_name(Some(DESCRIPTOR.icon)),
        }
        self.cover.set_pixel_size(COVER_SIZE);
    }
}

/// The bar position right now: the snapshot plus however long has passed since
/// it was taken, while the player is running.
fn progress_now(player: &Player) -> f64 {
    let position = if player.state == State::Playing {
        player.position.saturating_add(elapsed_micros(player.at))
    } else {
        player.position
    };
    progress_fraction(position, player.length)
}

fn elapsed_micros(at: Instant) -> i64 {
    i64::try_from(at.elapsed().as_micros()).unwrap_or(i64::MAX)
}

fn build(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: MediaConfig = context.config();

    let header = TileHeader::new(DESCRIPTOR.icon, "メディア");

    let cover = gtk::Image::from_icon_name(DESCRIPTOR.icon);
    cover.set_pixel_size(COVER_SIZE);
    cover.add_css_class("edm-cover");
    cover.set_valign(gtk::Align::Center);

    let title = gtk::Label::new(None);
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_hexpand(true);

    let subtitle = gtk::Label::new(None);
    subtitle.add_css_class("caption");
    subtitle.add_css_class("dim-label");
    subtitle.set_xalign(0.0);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&subtitle);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    body.set_valign(gtk::Align::Center);
    body.set_vexpand(true);
    body.append(&cover);
    body.append(&text);

    let bar = gtk::ProgressBar::new();
    bar.add_css_class("edm-bar");
    bar.set_show_text(false);
    bar.set_valign(gtk::Align::Center);

    let previous = gtk::Button::from_icon_name("media-skip-backward-symbolic");
    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    let next = gtk::Button::from_icon_name("media-skip-forward-symbolic");
    for button in [&previous, &play, &next] {
        button.add_css_class("flat");
        button.set_valign(gtk::Align::Center);
    }
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.set_halign(gtk::Align::Center);
    controls.append(&previous);
    controls.append(&play);
    controls.append(&next);

    let empty = gtk::Label::new(Some("再生中のプレイヤーはありません"));
    empty.add_css_class("caption");
    empty.add_css_class("dim-label");
    empty.set_wrap(true);
    empty.set_xalign(0.0);
    empty.set_vexpand(true);
    empty.set_valign(gtk::Align::Center);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_vexpand(true);
    root.append(&header.root);
    root.append(&body);
    root.append(&bar);
    root.append(&controls);
    root.append(&empty);

    // A tick runs during `tick_seconds`, but the first answer only arrives after
    // a round trip, so the tile starts out saying that nothing is playing.
    body.set_visible(false);
    bar.set_visible(false);
    controls.set_visible(false);

    let live = Rc::new(Live {
        context: context.clone(),
        config,
        header,
        cover,
        title,
        subtitle,
        body,
        bar,
        controls,
        play,
        empty,
        player: RefCell::new(None),
        connection: RefCell::new(None),
        ticks: Cell::new(0),
        pending: Cell::new(false),
    });

    // The buttons hold a weak reference: `Live` owns them, so a strong one would
    // be a cycle the tile could never break out of.
    for (button, method) in [
        (&previous, "Previous"),
        (&live.play, "PlayPause"),
        (&next, "Next"),
    ] {
        let weak = Rc::downgrade(&live);
        button.connect_clicked(move |_| {
            if let Some(live) = weak.upgrade() {
                live.command(method);
            }
        });
    }

    let live_for_tick = live.clone();
    super::tick_seconds(&root, TICK_SECONDS, move || live_for_tick.refresh());

    Ok(root.upcast())
}

/// The fact rows the detail view fills in when the bus answers. They are added
/// straight away so the dialog opens instantly.
struct Rows {
    player: libadwaita::ActionRow,
    state: libadwaita::ActionRow,
    track: libadwaita::ActionRow,
    artists: libadwaita::ActionRow,
    album: libadwaita::ActionRow,
    length: libadwaita::ActionRow,
    position: libadwaita::ActionRow,
    found: libadwaita::ActionRow,
}

impl Rows {
    fn new(page: &DetailPage) -> Rc<Self> {
        let row = |title: &str| {
            let row = libadwaita::ActionRow::builder()
                .title(title)
                .subtitle(PENDING)
                .build();
            page.fact_row(&row);
            row
        };
        Rc::new(Self {
            player: row("プレイヤー"),
            state: row("状態"),
            track: row("トラック"),
            artists: row("アーティスト"),
            album: row("アルバム"),
            length: row("長さ"),
            position: row("位置"),
            found: row("見つかっているプレイヤー"),
        })
    }

    fn show(&self, player: &Player, found: &[String]) {
        self.player.set_subtitle(label_or_dash(&player.identity));
        self.state.set_subtitle(state_label(player.state));
        self.track.set_subtitle(label_or_dash(&player.title));
        self.artists.set_subtitle(label_or_dash(&player.artist));
        self.album.set_subtitle(label_or_dash(&player.album));
        self.length.set_subtitle(&format_time(player.length));
        self.position.set_subtitle(&format_time(player.position));
        let names = found.join(", ");
        self.found
            .set_subtitle(if names.is_empty() { DASH } else { &names });
    }

    fn none(&self) {
        for row in [
            &self.state,
            &self.track,
            &self.artists,
            &self.album,
            &self.length,
            &self.position,
        ] {
            row.set_subtitle(DASH);
        }
        self.player.set_subtitle("再生中のプレイヤーはありません");
        self.found.set_subtitle(DASH);
    }
}

fn state_label(state: State) -> &'static str {
    match state {
        State::Playing => "再生中",
        State::Paused => "一時停止",
        State::Stopped => "停止",
    }
}

fn detail(context: &WidgetContext) -> Result<gtk::Widget> {
    let config: MediaConfig = context.config();
    let page = DetailPage::new("再生中のプレイヤー", "表示");
    let rows = Rows::new(&page);

    // `bus_get_sync` only ever returns a cached connection here: the tile has
    // been polling since it was placed, so this does not wait on the bus. It is
    // needed synchronously because the note below has to exist before `finish`.
    match Bus::session() {
        Some(bus) => {
            let wanted = config.player.clone();
            let rows_for_bus = rows;
            let handle = bus.clone();
            handle.list_names(move |names| {
                let names = names.unwrap_or_default();
                let players: Vec<String> = names
                    .into_iter()
                    .filter(|name| is_player_name(name))
                    .collect();
                let candidates: Vec<String> = players
                    .iter()
                    .filter(|name| matches_player(bus_suffix(name), &wanted))
                    .take(MAX_PLAYERS)
                    .cloned()
                    .collect();
                let found: Vec<String> = players
                    .iter()
                    .map(|name| bus_suffix(name).to_owned())
                    .collect();

                let rows_for_bus = rows_for_bus.clone();
                let on_done: OnPlayer = Rc::new(move |result| match result {
                    Some((_, player)) => rows_for_bus.show(&player, &found),
                    None => rows_for_bus.none(),
                });
                // Probes every candidate in parallel and reports the best of
                // them; the row in the page lists the whole bus either way.
                probe_best(&bus, candidates, on_done);
            });
        }
        None => {
            rows.none();
            page.note("セッションバスに接続できませんでした。MPRIS 対応のプレイヤーが動いているか確認してください。");
        }
    }

    page.switch(
        context,
        "only_while_playing",
        "再生中だけ表示",
        config.only_while_playing,
    );
    page.switch(
        context,
        "show_controls",
        "操作ボタンを表示",
        config.show_controls,
    );
    page.switch(context, "show_art", "アートワークを表示", config.show_art);
    page.entry(context, "player", "プレイヤー (空で自動)", &config.player);
    page.note("MPRIS (org.mpris.MediaPlayer2) に対応したプレイヤーが対象です。");
    page.note("プレイヤー名はバス名の末尾で照合します (例: spotify, firefox, mpv)。");
    Ok(page.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the `a{sv}` dictionary the bus would send.
    fn dictionary(entries: &[(&str, Variant)]) -> Variant {
        let map: HashMap<String, Variant> = entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect();
        map.to_variant()
    }

    fn player_properties(status: &str, metadata: Variant) -> Variant {
        dictionary(&[
            ("PlaybackStatus", status.to_variant()),
            ("Metadata", metadata),
        ])
    }

    fn metadata() -> Variant {
        dictionary(&[
            ("xesam:title", "夜に駆ける".to_variant()),
            ("xesam:artist", vec!["YOASOBI".to_owned()].to_variant()),
            ("xesam:album", "THE BOOK".to_variant()),
            ("mpris:length", 184_000_000i64.to_variant()),
            ("mpris:artUrl", "file:///tmp/a.jpg".to_variant()),
        ])
    }

    #[test]
    fn a_method_reply_is_unwrapped_from_its_return_value() {
        // D-Bus hands back `(as)` for ListNames. Asking gio to *expect* `as`
        // fails the whole call, which is exactly how this widget once ended up
        // finding no player at all — so both halves are pinned here.
        let names = vec![
            ":1.2".to_owned(),
            "org.mpris.MediaPlayer2.spotify".to_owned(),
        ];
        let reply = (names.clone(),).to_variant();
        assert_eq!(reply.type_().as_str(), "(as)");
        assert_eq!(payload(reply).get::<Vec<String>>(), Some(names));

        // GetAll answers `(a{sv})`, and the dictionary inside is what the
        // parser reads.
        let properties: HashMap<String, Variant> = [
            ("PlaybackStatus".to_owned(), "Playing".to_variant()),
            ("Metadata".to_owned(), metadata()),
        ]
        .into_iter()
        .collect();
        let wrapped = (properties,).to_variant();
        assert_eq!(wrapped.type_().as_str(), "(a{sv})");

        let player = parse_player("org.mpris.MediaPlayer2.spotify", &payload(wrapped));
        assert_eq!(player.state, State::Playing);
        assert_eq!(player.title, "夜に駆ける");
        assert_eq!(player.artist, "YOASOBI");
        assert_eq!(player.length, 184_000_000);

        // A command reply is empty, and a dictionary with a single entry is not
        // a reply at all: neither may be unwrapped.
        assert_eq!(payload(().to_variant()).n_children(), 0);
        let single = dictionary(&[("PlaybackStatus", "Playing".to_variant())]);
        assert_eq!(payload(single.clone()).type_(), single.type_());
    }

    #[test]
    fn bus_names_are_split_into_suffixes() {
        assert_eq!(bus_suffix("org.mpris.MediaPlayer2.spotify"), "spotify");
        assert_eq!(bus_suffix("org.mpris.MediaPlayer2."), "");
        assert_eq!(bus_suffix("org.freedesktop.DBus"), "org.freedesktop.DBus");

        assert!(is_player_name("org.mpris.MediaPlayer2.spotify"));
        assert!(is_player_name("org.mpris.MediaPlayer2.firefox.instance1"));
        assert!(!is_player_name("org.mpris.MediaPlayer2."));
        assert!(!is_player_name("org.freedesktop.DBus"));
        assert!(!is_player_name(""));
    }

    #[test]
    fn players_match_the_configured_prefix() {
        assert!(matches_player("spotify", ""));
        assert!(matches_player("spotify", "  "));
        assert!(matches_player("spotify", "spot"));
        assert!(matches_player("Spotify", "spot"));
        assert!(!matches_player("firefox", "spot"));
        assert!(!matches_player("", "spot"));
    }

    #[test]
    fn playback_statuses_become_states() {
        assert_eq!(playback_state("Playing"), State::Playing);
        assert_eq!(playback_state("Paused"), State::Paused);
        assert_eq!(playback_state("Stopped"), State::Stopped);
        assert_eq!(playback_state("playing"), State::Playing);
        assert_eq!(playback_state(""), State::Stopped);
        assert_eq!(playback_state("nonsense"), State::Stopped);
        assert!(State::Playing.rank() > State::Paused.rank());
        assert!(State::Paused.rank() > State::Stopped.rank());
    }

    #[test]
    fn times_are_formatted_as_minutes_and_seconds() {
        assert_eq!(format_time(184_000_000), "3:04");
        assert_eq!(format_time(0), "--:--");
        assert_eq!(format_time(-1), "--:--");
        assert_eq!(format_time(1_000_000), "0:01");
        assert_eq!(format_time(59_000_000), "0:59");
        assert_eq!(format_time(3_600_000_000), "1:00:00");
        assert_eq!(format_time(3_723_000_000), "1:02:03");
    }

    #[test]
    fn progress_is_clamped_between_zero_and_one() {
        assert_eq!(progress_fraction(30_000_000, 120_000_000), 0.25);
        assert_eq!(progress_fraction(0, 120_000_000), 0.0);
        assert_eq!(progress_fraction(240_000_000, 120_000_000), 1.0);
        assert_eq!(progress_fraction(-5, 120_000_000), 0.0);
        assert_eq!(progress_fraction(5, 0), 0.0);
        assert_eq!(progress_fraction(5, -1), 0.0);
    }

    #[test]
    fn only_file_urls_are_read_from_disk() {
        assert!(is_local_art("file:///tmp/a.jpg"));
        assert!(!is_local_art("https://example.com/a.jpg"));
        assert!(!is_local_art("data:image/png;base64,AAAA"));
        assert!(!is_local_art(""));

        assert_eq!(
            local_art_path("file:///tmp/a.jpg"),
            Some(PathBuf::from("/tmp/a.jpg"))
        );
        assert_eq!(
            local_art_path("file:///tmp/a%20b.jpg"),
            Some(PathBuf::from("/tmp/a b.jpg"))
        );
        assert_eq!(local_art_path("https://example.com/a.jpg"), None);
        assert_eq!(local_art_path("file://"), None);
    }

    #[test]
    fn metadata_is_read_through_the_variant_dictionary() {
        let metadata = metadata();
        assert_eq!(
            metadata_text(&metadata, "xesam:title").as_deref(),
            Some("夜に駆ける")
        );
        assert_eq!(
            metadata_list(&metadata, "xesam:artist"),
            Some(vec!["YOASOBI".to_owned()])
        );
        assert_eq!(metadata_int(&metadata, "mpris:length"), Some(184_000_000));
        assert_eq!(
            metadata_text(&metadata, "mpris:artUrl").as_deref(),
            Some("file:///tmp/a.jpg")
        );

        // Missing keys, unexpected types and blank strings are all "unknown".
        assert_eq!(metadata_text(&metadata, "xesam:genre"), None);
        assert_eq!(metadata_int(&metadata, "xesam:title"), None);
        assert_eq!(metadata_list(&metadata, "xesam:title"), None);
        assert_eq!(metadata_text(&metadata, ""), None);

        let blank = dictionary(&[("xesam:title", "   ".to_variant())]);
        assert_eq!(metadata_text(&blank, "xesam:title"), None);

        // Not a dictionary at all.
        assert_eq!(metadata_text(&7i64.to_variant(), "xesam:title"), None);
        assert!(dictionary(&[]).get::<HashMap<String, Variant>>().is_some());
    }

    #[test]
    fn numbers_of_any_width_are_accepted() {
        assert_eq!(variant_i64(&5i64.to_variant()), Some(5));
        assert_eq!(variant_i64(&5u64.to_variant()), Some(5));
        assert_eq!(variant_i64(&5i32.to_variant()), Some(5));
        assert_eq!(variant_i64(&5u32.to_variant()), Some(5));
        assert_eq!(variant_i64(&5i16.to_variant()), Some(5));
        assert_eq!(variant_i64(&5u16.to_variant()), Some(5));
        assert_eq!(variant_i64(&u64::MAX.to_variant()), None);
        assert_eq!(variant_i64(&"5".to_variant()), None);
    }

    #[test]
    fn players_are_parsed_from_their_properties() {
        let player = parse_player(
            "org.mpris.MediaPlayer2.spotify",
            &player_properties("Playing", metadata()),
        );
        assert_eq!(player.identity, "spotify");
        assert_eq!(player.state, State::Playing);
        assert_eq!(player.title, "夜に駆ける");
        assert_eq!(player.artist, "YOASOBI");
        assert_eq!(player.album, "THE BOOK");
        assert_eq!(player.length, 184_000_000);
        assert_eq!(player.art_url, "file:///tmp/a.jpg");
        assert_eq!(player.position, 0);

        // A player with no metadata and no status is merely stopped.
        let bare = parse_player("org.mpris.MediaPlayer2.mpv", &dictionary(&[]));
        assert_eq!(bare.state, State::Stopped);
        assert_eq!(bare.title, "");
        assert_eq!(bare.length, 0);

        let odd = parse_player(
            "org.mpris.MediaPlayer2.vlc",
            &player_properties("Playing", "not a dictionary".to_variant()),
        );
        assert_eq!(odd.state, State::Playing);
        assert_eq!(odd.title, "");
    }

    #[test]
    fn durations_only_count_down_while_playing() {
        let mut player = Player {
            identity: "spotify".to_owned(),
            state: State::Playing,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            length: 60_000_000,
            art_url: String::new(),
            position: 30_000_000,
            at: Instant::now(),
        };
        assert!(progress_now(&player) >= 0.5);

        player.state = State::Paused;
        assert_eq!(progress_now(&player), 0.5);

        player.state = State::Playing;
        player.length = 0;
        assert_eq!(progress_now(&player), 0.0);
        assert!(elapsed_micros(player.at) >= 0);
    }

    #[test]
    fn missing_values_read_as_an_unknown_track() {
        assert_eq!(label_or_dash(""), DASH);
        assert_eq!(label_or_dash("  "), DASH);
        assert_eq!(label_or_dash("title"), "title");

        assert_eq!(track_line("YOASOBI", "THE BOOK"), "YOASOBI · THE BOOK");
        assert_eq!(track_line("YOASOBI", ""), "YOASOBI");
        assert_eq!(track_line("", "THE BOOK"), "THE BOOK");
        assert_eq!(track_line(" ", " "), DASH);
    }
}
