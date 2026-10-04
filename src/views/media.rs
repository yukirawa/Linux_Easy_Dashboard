//! `media` view family. See `src/views/mod.rs`.
//!
//! Media player tile, driven by MPRIS over the session bus.
//!
//! * Players are found by listing the session bus and keeping the names under
//!   `org.mpris.MediaPlayer2.`, so any MPRIS capable player works — Spotify,
//!   Firefox, mpv, VLC.
//! * Every call is asynchronous, so a player that hangs or quits cannot freeze
//!   the dashboard. Errors are swallowed and read as "nothing is playing".
//! * `Position` is a snapshot taken when the properties arrive, so the bar is
//!   advanced locally between the once-a-second polls.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use anyhow::Result;
use glib::Variant;
use gtk::prelude::*;
use gtk::{gio, glib};
use log::debug;

use crate::plugin::{Plugin, View};
use crate::widgets::{TileHeader, WidgetContext, tick_seconds};

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
/// Shown in place of a missing value.
const DASH: &str = "—";

/// The `[view]` options this tile draws with, taken from the definition.
struct Options {
    /// Bus name suffix of the player to follow. Empty follows whatever plays.
    player: String,
    /// Hide the tile when nothing is playing.
    only_while_playing: bool,
    show_controls: bool,
    show_art: bool,
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

/// The tile's live state, kept in one place so the tick, the callbacks and the
/// buttons stay in sync.
struct Live {
    context: WidgetContext,
    config: Options,
    icon: String,
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

    /// Puts the artwork up when the player offers a local file, and the tile's
    /// own icon otherwise (web URLs are never fetched).
    fn set_cover(&self, art_url: &str) {
        let path = if self.config.show_art {
            local_art_path(art_url)
        } else {
            None
        };
        match path {
            Some(path) => self.cover.set_from_file(Some(path)),
            None => self.cover.set_icon_name(Some(&self.icon)),
        }
        self.cover.set_pixel_size(COVER_SIZE);
    }
}

pub fn media(definition: &Plugin, context: &WidgetContext) -> Result<gtk::Widget> {
    let config = match &definition.view {
        View::Media {
            player,
            only_while_playing,
            show_controls,
            show_art,
        } => Options {
            player: player.clone(),
            only_while_playing: *only_while_playing,
            show_controls: *show_controls,
            show_art: *show_art,
        },
        _ => unreachable!("media called with the wrong view"),
    };
    let icon = if definition.icon.trim().is_empty() {
        "audio-x-generic-symbolic".to_owned()
    } else {
        definition.icon.clone()
    };

    let header = TileHeader::new(&icon, "メディア");

    let cover = gtk::Image::from_icon_name(&icon);
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
        icon,
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
    tick_seconds(&root, TICK_SECONDS, move || live_for_tick.refresh());

    Ok(root.upcast())
}
