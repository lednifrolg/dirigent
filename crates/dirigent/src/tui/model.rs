//! The TUI's state, and what each message does to it; [`super::view`] draws it.
//!
//! The model does no I/O: it answers messages with [`Effect`]s, which [`super`] runs in the
//! background and answers with more messages.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::Duration;

use dirigera::{
    Backoff, Client, Device, DeviceId, DeviceKind, Event, EventKind, Home, PRESS_WINDOW, Room,
    RoomId, Scene, SceneId,
};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ratatui::crossterm::event::KeyEvent;

use super::keymap::{self, Action};
use crate::theme;

/// Activity lines kept; the pane shows the newest.
const ACTIVITY_KEPT: usize = 100;
/// A state change this soon after a remote's press is taken to come from that remote.
const PRESS_LINK: SignedDuration = SignedDuration::from_millis(1500);
/// The same line again within this time is not repeated: the hub reports some things twice.
const REPEAT: SignedDuration = SignedDuration::from_secs(2);

pub enum Msg {
    Key(KeyEvent),
    Tick(Timestamp),
    /// The paired hub answered, or why not.
    Connected(Result<Connected, Failure>),
    /// The event stream is open.
    Live,
    Event(Box<Event>),
    /// The event stream ended, and why.
    Lost(String),
    /// The home, read again.
    Resynced(Result<Home, Failure>),
    /// Progress of a pairing.
    Pairing(Pairing),
    /// A click on a row of the list.
    Select(Row),
    /// A click on a tab.
    Tab(Tab),
}

/// A hub that answered.
pub struct Connected {
    pub client: Client,
    pub home: Home,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    NotPaired,
    /// The hub no longer accepts dirigent, or presented another certificate.
    PairAgain,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

pub enum Pairing {
    /// A hub gave out a code and waits for its button.
    Pressing {
        hub: String,
        address: SocketAddr,
    },
    Done(Box<Connected>),
    Failed(String),
}

/// Background work the TUI asks for.
#[derive(Debug)]
pub enum Effect {
    Connect,
    /// Opens the event stream after `after`; with `resync`, reads the home first.
    Listen {
        client: Client,
        after: Duration,
        resync: bool,
    },
    Resync {
        client: Client,
    },
    Pair,
    CancelPairing,
}

/// What can be selected in the list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Row {
    Room(RoomId),
    Device(DeviceId),
    Scene(SceneId),
}

/// A line of the device list: rooms with their devices, then scenes, then remotes.
#[derive(Debug, Clone, Copy)]
pub enum Line<'a> {
    Room(&'a Room),
    Device(&'a Device),
    Heading(&'static str),
    Scene(&'a Scene),
    Remote(&'a Device),
}

impl Line<'_> {
    pub fn row(&self) -> Option<Row> {
        match self {
            Self::Room(room) => Some(Row::Room(room.id.clone())),
            Self::Device(device) | Self::Remote(device) => Some(Row::Device(device.id.clone())),
            Self::Scene(scene) => Some(Row::Scene(scene.id.clone())),
            Self::Heading(_) => None,
        }
    }
}

/// "1 scene", "2 scenes".
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn switchable(device: &Device) -> bool {
    !matches!(device.kind(), DeviceKind::Controller | DeviceKind::Gateway)
}

pub fn lines(home: &Home) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    for room in &home.rooms {
        lines.push(Line::Room(room));
        lines.extend(
            home.devices_in(&room.id)
                .filter(|d| switchable(d))
                .map(Line::Device),
        );
    }
    let roomless: Vec<&Device> = home
        .devices
        .iter()
        .filter(|d| switchable(d) && d.room.is_none())
        .collect();
    if !roomless.is_empty() {
        lines.push(Line::Heading("Other devices"));
        lines.extend(roomless.into_iter().map(Line::Device));
    }
    if !home.scenes.is_empty() {
        lines.push(Line::Heading("Scenes"));
        lines.extend(home.scenes.iter().map(Line::Scene));
    }
    let remotes: Vec<&Device> = home
        .devices
        .iter()
        .filter(|d| d.kind() == DeviceKind::Controller)
        .collect();
    if !remotes.is_empty() {
        lines.push(Line::Heading("Remotes"));
        lines.extend(remotes.into_iter().map(Line::Remote));
    }
    lines
}

/// Tabs of the narrow layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Devices,
    Activity,
}

impl Tab {
    pub const ALL: [Self; 2] = [Self::Devices, Self::Activity];

    pub fn name(self) -> &'static str {
        match self {
            Self::Devices => "devices",
            Self::Activity => "activity",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Reading the paired hub for the first time.
    Starting,
    Ready,
    /// Not paired yet, or the hub no longer accepts dirigent.
    Pairing(Dialog),
    /// The hub could not be reached; trying again at `retry_at`.
    Unreachable {
        message: String,
        retry_at: Timestamp,
    },
}

/// The pairing dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    /// Looking for the hub; `reason` says why pairing is needed again.
    Searching {
        reason: Option<String>,
    },
    Pressing {
        hub: String,
        address: SocketAddr,
        until: Timestamp,
    },
    Failed(String),
}

/// The event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Offline { retry_at: Timestamp },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Info,
    Warn,
    Bad,
    Press,
}

/// A line of the activity pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub at: Timestamp,
    pub tone: Tone,
    pub who: String,
    pub what: String,
    /// Where a change came from, when it is known.
    pub source: Option<String>,
}

/// Everything the TUI starts with.
pub struct Start {
    pub now: Timestamp,
    pub tz: TimeZone,
    /// Lights may be drawn in their own color (no `NO_COLOR`).
    pub colors: bool,
}

pub struct Model {
    pub now: Timestamp,
    pub tz: TimeZone,
    pub colors: bool,
    pub phase: Phase,
    pub link: Link,
    pub home: Option<Home>,
    pub hub_name: Option<String>,
    pub address: Option<SocketAddr>,
    pub selected: Option<Row>,
    pub tab: Tab,
    pub overlay: Overlay,
    /// Newest first.
    pub activity: VecDeque<Entry>,
    /// When each remote was last pressed, as far as the event stream told.
    pub pressed: HashMap<DeviceId, Timestamp>,
    pub quit: bool,
    client: Option<Client>,
    backoff: Backoff,
    /// The latest remote press, to name it as the source of what follows.
    last_press: Option<(String, Timestamp)>,
}

impl Model {
    pub fn start(start: Start) -> (Self, Vec<Effect>) {
        let model = Self {
            now: start.now,
            tz: start.tz,
            colors: start.colors,
            phase: Phase::Starting,
            link: Link::Connecting,
            home: None,
            hub_name: None,
            address: None,
            selected: None,
            tab: Tab::Devices,
            overlay: Overlay::None,
            activity: VecDeque::new(),
            pressed: HashMap::new(),
            quit: false,
            client: None,
            backoff: Backoff::default(),
            last_press: None,
        };
        (model, vec![Effect::Connect])
    }

    /// Whether something moves on screen: a spinner while connecting or searching.
    pub fn spinning(&self) -> bool {
        matches!(
            self.phase,
            Phase::Starting | Phase::Pairing(Dialog::Searching { .. })
        ) || (self.phase == Phase::Ready && self.link == Link::Connecting)
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => keymap::action(key).map_or_else(Vec::new, |action| self.act(action)),
            Msg::Tick(now) => {
                self.now = now;
                match &self.phase {
                    Phase::Unreachable { retry_at, .. } if now >= *retry_at => {
                        self.phase = Phase::Starting;
                        vec![Effect::Connect]
                    }
                    _ => vec![],
                }
            }
            Msg::Connected(Ok(connected)) => self.connected(connected, "connected"),
            Msg::Connected(Err(failure)) => self.failed(failure),
            Msg::Live => {
                if matches!(self.link, Link::Offline { .. }) {
                    self.log(Tone::Good, "hub", "reconnected", None);
                }
                self.link = Link::Live;
                self.backoff.reset();
                vec![]
            }
            Msg::Event(event) => self.event(&event),
            Msg::Lost(reason) => {
                let Some(client) = self.client.clone() else {
                    return vec![];
                };
                let delay = self.backoff.next_delay();
                self.link = Link::Offline {
                    retry_at: self.now + SignedDuration::try_from(delay).unwrap_or_default(),
                };
                self.log(
                    Tone::Warn,
                    "hub",
                    &format!("connection lost: {reason}"),
                    None,
                );
                vec![Effect::Listen {
                    client,
                    after: delay,
                    resync: true,
                }]
            }
            Msg::Resynced(Ok(home)) => {
                self.set_home(home);
                self.log(Tone::Good, "hub", "state read again", None);
                vec![]
            }
            Msg::Resynced(Err(failure)) if failure.kind != FailureKind::Other => {
                self.failed(failure)
            }
            Msg::Resynced(Err(failure)) => {
                self.log(Tone::Bad, "hub", &failure.message, None);
                vec![]
            }
            Msg::Pairing(pairing) => self.pairing(pairing),
            Msg::Select(row) => {
                if self.overlay == Overlay::None {
                    self.selected = Some(row);
                }
                vec![]
            }
            Msg::Tab(tab) => {
                self.tab = tab;
                vec![]
            }
        }
    }

    fn act(&mut self, action: Action) -> Vec<Effect> {
        if action == Action::Quit {
            self.quit = true;
            return vec![Effect::CancelPairing];
        }
        if self.overlay == Overlay::Help {
            if matches!(action, Action::Help | Action::Back) {
                self.overlay = Overlay::None;
            }
            return vec![];
        }
        match &self.phase {
            Phase::Pairing(dialog) => {
                return match (dialog, action) {
                    (Dialog::Failed(_), Action::Confirm) => {
                        self.phase = Phase::Pairing(Dialog::Searching { reason: None });
                        vec![Effect::Pair]
                    }
                    (Dialog::Searching { .. } | Dialog::Pressing { .. }, Action::Back) => {
                        self.phase = Phase::Pairing(Dialog::Failed("pairing cancelled".into()));
                        vec![Effect::CancelPairing]
                    }
                    _ => vec![],
                };
            }
            Phase::Unreachable { .. } => {
                if matches!(action, Action::Confirm | Action::Resync) {
                    self.phase = Phase::Starting;
                    return vec![Effect::Connect];
                }
                return vec![];
            }
            Phase::Starting | Phase::Ready => {}
        }
        match action {
            Action::Up => self.step(-1),
            Action::Down => self.step(1),
            Action::NextTab => {
                self.tab = match self.tab {
                    Tab::Devices => Tab::Activity,
                    Tab::Activity => Tab::Devices,
                };
            }
            Action::Tab(index) => {
                if let Some(&tab) = Tab::ALL.get(index) {
                    self.tab = tab;
                }
            }
            Action::Resync => {
                if let Some(client) = self.client.clone() {
                    return vec![Effect::Resync { client }];
                }
            }
            Action::Help => self.overlay = Overlay::Help,
            Action::Quit | Action::Confirm | Action::Back => {}
        }
        vec![]
    }

    /// Moves the selection `delta` rows, wrapping around.
    fn step(&mut self, delta: isize) {
        let Some(home) = &self.home else {
            return;
        };
        let rows: Vec<Row> = lines(home).iter().filter_map(Line::row).collect();
        if rows.is_empty() {
            return;
        }
        let count = rows.len().cast_signed();
        let at = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| row == selected))
            .map_or(if delta > 0 { -1 } else { 0 }, usize::cast_signed);
        let next = (at + delta).rem_euclid(count);
        self.selected = Some(rows[next.cast_unsigned()].clone());
    }

    fn connected(&mut self, connected: Connected, how: &str) -> Vec<Effect> {
        let Connected { client, home, name } = connected;
        let devices = home.devices.iter().filter(|d| switchable(d)).count();
        let remotes = home
            .devices
            .iter()
            .filter(|d| d.kind() == DeviceKind::Controller)
            .count();
        let scenes = home.scenes.len();
        self.log(
            Tone::Good,
            "hub",
            &format!(
                "{how} · {} · {} · {}",
                count(devices, "device"),
                count(remotes, "remote"),
                count(scenes, "scene")
            ),
            None,
        );
        self.address = Some(client.address());
        self.hub_name = Some(name);
        self.set_home(home);
        self.phase = Phase::Ready;
        self.link = Link::Connecting;
        self.backoff.reset();
        self.client = Some(client.clone());
        vec![Effect::Listen {
            client,
            after: Duration::ZERO,
            resync: false,
        }]
    }

    fn failed(&mut self, failure: Failure) -> Vec<Effect> {
        match failure.kind {
            FailureKind::NotPaired => {
                self.phase = Phase::Pairing(Dialog::Searching { reason: None });
                vec![Effect::Pair]
            }
            FailureKind::PairAgain => {
                self.client = None;
                self.phase = Phase::Pairing(Dialog::Searching {
                    reason: Some(failure.message),
                });
                vec![Effect::Pair]
            }
            FailureKind::Other => {
                let delay = self.backoff.next_delay();
                self.phase = Phase::Unreachable {
                    message: failure.message,
                    retry_at: self.now + SignedDuration::try_from(delay).unwrap_or_default(),
                };
                vec![]
            }
        }
    }

    fn pairing(&mut self, pairing: Pairing) -> Vec<Effect> {
        // Late news from a pairing the user cancelled.
        if !matches!(
            self.phase,
            Phase::Pairing(Dialog::Searching { .. } | Dialog::Pressing { .. })
        ) {
            return vec![];
        }
        match pairing {
            Pairing::Pressing { hub, address } => {
                self.phase = Phase::Pairing(Dialog::Pressing {
                    hub,
                    address,
                    until: self.now + SignedDuration::try_from(PRESS_WINDOW).unwrap_or_default(),
                });
                vec![]
            }
            Pairing::Done(connected) => self.connected(*connected, "paired"),
            Pairing::Failed(message) => {
                self.phase = Phase::Pairing(Dialog::Failed(message));
                vec![]
            }
        }
    }

    /// Takes in a new home, keeping the selection when it still exists.
    fn set_home(&mut self, home: Home) {
        let rows: Vec<Row> = lines(&home).iter().filter_map(Line::row).collect();
        if self.selected.as_ref().is_none_or(|row| !rows.contains(row)) {
            // The first device, rather than its room.
            self.selected = rows
                .iter()
                .find(|row| matches!(row, Row::Device(_)))
                .or_else(|| rows.first())
                .cloned();
        }
        self.home = Some(home);
    }

    fn event(&mut self, event: &Event) -> Vec<Effect> {
        let Some(home) = self.home.as_mut() else {
            return vec![];
        };
        let at = event.time.unwrap_or(self.now);
        match &event.kind {
            EventKind::DeviceStateChanged(update) => {
                if !home.apply(update) {
                    // A device this TUI does not know yet: read everything again.
                    return self
                        .client
                        .clone()
                        .map(|client| Effect::Resync { client })
                        .into_iter()
                        .collect();
                }
                let Some(device) = home.device(&update.id) else {
                    return vec![];
                };
                let name = device.name().to_owned();
                let remote = device.kind() == DeviceKind::Controller;
                let what = theme::describe_update(update);
                if remote {
                    self.pressed.insert(update.id.clone(), at);
                    self.last_press = Some((name.clone(), at));
                }
                if what.is_empty() {
                    return vec![];
                }
                let source = if remote {
                    None
                } else {
                    Some(
                        self.last_press
                            .as_ref()
                            .filter(|(_, pressed)| at.duration_since(*pressed) <= PRESS_LINK)
                            .map_or_else(|| "elsewhere".to_owned(), |(remote, _)| remote.clone()),
                    )
                };
                self.log_at(at, Tone::Info, &name, &what, source);
            }
            EventKind::RemotePress { device, pattern } => {
                let name = home
                    .device(device)
                    .map_or_else(|| device.to_string(), |d| d.name().to_owned());
                self.pressed.insert(device.clone(), at);
                self.last_press = Some((name.clone(), at));
                self.log_at(at, Tone::Press, &name, pattern, None);
            }
            EventKind::Scene(id) => {
                let name = home
                    .scene(id)
                    .map_or_else(|| id.to_string(), |s| s.name().to_owned());
                let what = event.event_type.trim_start_matches("scene").to_lowercase();
                self.log_at(at, Tone::Info, &format!("scene {name}"), &what, None);
            }
            _ => {}
        }
        vec![]
    }

    fn log(&mut self, tone: Tone, who: &str, what: &str, source: Option<String>) {
        self.log_at(self.now, tone, who, what, source);
    }

    fn log_at(&mut self, at: Timestamp, tone: Tone, who: &str, what: &str, source: Option<String>) {
        let repeated = self.activity.front().is_some_and(|last| {
            last.who == who && last.what == what && at.duration_since(last.at) < REPEAT
        });
        if repeated {
            return;
        }
        self.activity.push_front(Entry {
            at,
            tone,
            who: who.to_owned(),
            what: what.to_owned(),
            source,
        });
        self.activity.truncate(ACTIVITY_KEPT);
    }

    pub fn device(&self, id: &DeviceId) -> Option<&Device> {
        self.home.as_ref()?.device(id)
    }
}

#[cfg(test)]
pub mod tests {
    use std::net::SocketAddr;

    use dirigera::Token;
    use ratatui::crossterm::event::KeyCode;
    use serde_json::json;

    use super::*;

    pub const HOME: &str = include_str!("../../../dirigera/tests/data/home.json");
    pub const STRIP: &str = "a1b2c3d4-0000-4000-8000-000000000001_1";
    pub const LAMP: &str = "a1b2c3d4-0000-4000-8000-000000000002_1";
    pub const REMOTE: &str = "a1b2c3d4-0000-4000-8000-000000000003_1";

    pub fn now() -> Timestamp {
        "2026-01-01T12:00:00Z".parse().unwrap()
    }

    pub fn home() -> Home {
        serde_json::from_str(HOME).unwrap()
    }

    pub fn connected() -> Connected {
        let address: SocketAddr = "192.168.1.20:8443".parse().unwrap();
        Connected {
            client: Client::plain(address, Token::new("TOKEN")).unwrap(),
            home: home(),
            name: "Test hub".to_owned(),
        }
    }

    pub fn start() -> Start {
        Start {
            now: now(),
            tz: TimeZone::UTC,
            colors: true,
        }
    }

    /// Connected, with the event stream open.
    pub fn live() -> Model {
        let (mut model, _) = Model::start(start());
        model.update(Msg::Connected(Ok(connected())));
        model.update(Msg::Live);
        model
    }

    pub fn key(code: KeyCode) -> Msg {
        Msg::Key(code.into())
    }

    pub fn event(value: &serde_json::Value) -> Msg {
        Msg::Event(Box::new(Event::parse(&value.to_string()).unwrap()))
    }

    #[test]
    fn it_connects_first_and_listens_once_connected() {
        let (mut model, effects) = Model::start(start());
        assert!(matches!(effects.as_slice(), [Effect::Connect]));
        assert_eq!(model.phase, Phase::Starting);
        let effects = model.update(Msg::Connected(Ok(connected())));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Listen { resync: false, .. }]
        ));
        assert_eq!(model.phase, Phase::Ready);
        assert_eq!(model.link, Link::Connecting);
        assert_eq!(model.selected, Some(Row::Device(DeviceId(STRIP.into()))));
        model.update(Msg::Live);
        assert_eq!(model.link, Link::Live);
        assert_eq!(
            model.activity[0].what,
            "connected · 2 devices · 1 remote · 1 scene"
        );
    }

    #[test]
    fn the_selection_moves_over_rows_and_wraps() {
        let mut model = live();
        let names = |model: &mut Model, code, times: usize| -> Vec<String> {
            (0..times)
                .map(|_| {
                    model.update(key(code));
                    match model.selected.clone().unwrap() {
                        Row::Room(id) => {
                            model.home.as_ref().unwrap().room(&id).unwrap().name.clone()
                        }
                        Row::Device(id) => model.device(&id).unwrap().name().to_owned(),
                        Row::Scene(id) => model
                            .home
                            .as_ref()
                            .unwrap()
                            .scene(&id)
                            .unwrap()
                            .name()
                            .to_owned(),
                    }
                })
                .collect()
        };
        assert_eq!(
            names(&mut model, KeyCode::Down, 6),
            [
                "Bedroom",
                "Reading lamp",
                "Evening",
                "Bedside switch",
                "Study",
                "Shelf strip"
            ]
        );
        assert_eq!(
            names(&mut model, KeyCode::Char('k'), 2),
            ["Study", "Bedside switch"]
        );
    }

    #[test]
    fn changes_from_elsewhere_update_the_home_and_the_activity() {
        let mut model = live();
        model.update(event(&json!({
            "type": "deviceStateChanged",
            "time": "2026-01-01T12:00:05.000Z",
            "data": { "id": STRIP, "attributes": { "lightLevel": 40 } },
        })));
        assert_eq!(
            model
                .device(&DeviceId(STRIP.into()))
                .unwrap()
                .attributes
                .light_level(),
            Some(40)
        );
        let latest = &model.activity[0];
        assert_eq!(
            (latest.who.as_str(), latest.what.as_str()),
            ("Shelf strip", "brightness 40 %")
        );
        assert_eq!(latest.source.as_deref(), Some("elsewhere"));
    }

    #[test]
    fn what_follows_a_press_is_put_down_to_the_remote() {
        let mut model = live();
        model.update(event(&json!({
            "type": "remotePressEvent",
            "time": "2026-01-01T12:00:05.000Z",
            "data": { "id": REMOTE, "clickPattern": "singlePress" },
        })));
        model.update(event(&json!({
            "type": "deviceStateChanged",
            "time": "2026-01-01T12:00:05.400Z",
            "data": { "id": LAMP, "attributes": { "isOn": true } },
        })));
        assert_eq!(model.activity[0].source.as_deref(), Some("Bedside switch"));
        assert_eq!(model.activity[1].tone, Tone::Press);
        assert!(model.pressed.contains_key(&DeviceId(REMOTE.into())));
    }

    #[test]
    fn repeated_reports_show_once() {
        let mut model = live();
        let updated = json!({
            "type": "sceneUpdated",
            "time": "2026-01-01T12:00:05.000Z",
            "data": { "id": "a1b2c3d4-0000-4000-8000-0000000000b1" },
        });
        model.update(event(&updated));
        model.update(event(&updated));
        assert_eq!(model.activity.len(), 2, "{:?}", model.activity);
        assert_eq!(model.activity[0].who, "scene Evening");
    }

    #[test]
    fn a_lost_stream_reconnects_with_a_resync() {
        let mut model = live();
        let effects = model.update(Msg::Lost("the hub closed the event stream".into()));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Listen { after, resync: true, .. }] if *after == Duration::from_secs(1)
        ));
        assert!(matches!(model.link, Link::Offline { .. }));
        model.update(Msg::Live);
        assert_eq!(model.link, Link::Live);
        assert_eq!(model.activity[0].what, "reconnected");
    }

    #[test]
    fn without_a_pairing_the_dialog_starts_and_can_be_cancelled() {
        let (mut model, _) = Model::start(start());
        let failure = Failure {
            kind: FailureKind::NotPaired,
            message: "not paired".into(),
        };
        let effects = model.update(Msg::Connected(Err(failure)));
        assert!(matches!(effects.as_slice(), [Effect::Pair]));
        model.update(Msg::Pairing(Pairing::Pressing {
            hub: "gw2-0123456789ab".into(),
            address: "192.168.1.20:8443".parse().unwrap(),
        }));
        assert!(matches!(
            model.phase,
            Phase::Pairing(Dialog::Pressing { .. })
        ));
        let effects = model.update(key(KeyCode::Esc));
        assert!(matches!(effects.as_slice(), [Effect::CancelPairing]));
        assert_eq!(
            model.phase,
            Phase::Pairing(Dialog::Failed("pairing cancelled".into()))
        );
        // A late answer from the cancelled pairing changes nothing.
        model.update(Msg::Pairing(Pairing::Done(Box::new(connected()))));
        assert!(matches!(model.phase, Phase::Pairing(Dialog::Failed(_))));
        let effects = model.update(key(KeyCode::Enter));
        assert!(matches!(effects.as_slice(), [Effect::Pair]));
        model.update(Msg::Pairing(Pairing::Done(Box::new(connected()))));
        assert_eq!(model.phase, Phase::Ready);
        assert!(model.activity[0].what.starts_with("paired ·"));
    }

    #[test]
    fn an_unreachable_hub_is_tried_again() {
        let (mut model, _) = Model::start(start());
        model.update(Msg::Connected(Err(Failure {
            kind: FailureKind::Other,
            message: "could not reach the hub".into(),
        })));
        assert!(matches!(model.phase, Phase::Unreachable { .. }));
        assert!(model.update(Msg::Tick(now())).is_empty());
        let effects = model.update(Msg::Tick(now() + SignedDuration::from_secs(1)));
        assert!(matches!(effects.as_slice(), [Effect::Connect]));
        assert_eq!(model.phase, Phase::Starting);
    }

    #[test]
    fn help_opens_and_closes_and_q_quits() {
        let mut model = live();
        model.update(key(KeyCode::Char('?')));
        assert_eq!(model.overlay, Overlay::Help);
        model.update(key(KeyCode::Down));
        assert_eq!(
            model.selected,
            Some(Row::Device(DeviceId(STRIP.into()))),
            "keys go to the help"
        );
        model.update(key(KeyCode::Esc));
        assert_eq!(model.overlay, Overlay::None);
        model.update(key(KeyCode::Char('q')));
        assert!(model.quit);
    }
}
