//! Changing devices from the TUI: what a key asks for, the changes on their way, and their
//! confirmation by the hub's events.
//!
//! A change shows at once. It goes out after [`COALESCE`], together with whatever else was asked
//! of the same device meanwhile, so a held key sends a few requests a second and the latest value
//! always goes last. The device confirms a change with a state change that shows the wanted
//! value; a fading light reports other values first. After [`CONFIRM_WITHIN`] without one, the
//! device is read back.

use std::time::Duration;

use dirigera::{
    Attributes, Change, ColorMode, Device, DeviceId, DeviceKind, DeviceUpdate, Home, SceneId,
    TemperatureRange,
};
use jiff::{SignedDuration, Timestamp};

use super::super::keymap::Action;
use super::{Effect, Failure, FailureKind, Link, Model, Row, Tone};
use crate::control::{self, CONFIRM_WITHIN, Wanted};
use crate::names::Target;
use crate::theme;
use crate::wanted::{Level, PRESETS, White};

/// How long a change waits for others to the same device before it goes out.
pub const COALESCE: SignedDuration = SignedDuration::from_millis(120);
const IDENTIFY: Duration = Duration::from_secs(5);
/// Changes of the same kind this close together are one step for `u`: a held key.
const UNDO_MERGE: SignedDuration = SignedDuration::from_secs(2);
const UNDO_KEPT: usize = 50;
/// How long a brightness bar takes to run to a new value.
const BAR_MOTION: SignedDuration = SignedDuration::from_millis(250);
/// Steps of the mouse wheel, the color picker and its fine steps.
const WHEEL_BRIGHTNESS: i16 = 5;
const HUE_STEP: f64 = 5.0;
const SATURATION_STEP: f64 = 0.05;
const FINE_KELVIN: u32 = 10;

/// What a change touches: a newer change of the same kind replaces an older one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Power,
    Brightness,
    /// White or color: what the light shows.
    Shade,
}

fn kind(change: &Change) -> Kind {
    match change {
        Change::Power(_) => Kind::Power,
        Change::Brightness(_) => Kind::Brightness,
        Change::White(_) | Change::Color { .. } => Kind::Shade,
    }
}

/// The attributes the hub reports for a kind of change.
fn reported_as(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Power => &["isOn"],
        Kind::Brightness => &["lightLevel"],
        Kind::Shade => &[
            "colorTemperature",
            "colorHue",
            "colorSaturation",
            "colorMode",
        ],
    }
}

/// The attributes a change sets, with the color mode it puts a light in, as the hub reports them.
pub fn shows(change: &Change) -> Attributes {
    let mut attributes = change.attributes();
    match change {
        Change::White(_) => attributes.insert("colorMode", "temperature"),
        Change::Color { .. } => attributes.insert("colorMode", "color"),
        _ => {}
    }
    attributes
}

/// Where a change from here is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Waiting for other changes to the device until `due`.
    Queued {
        due: Timestamp,
    },
    Sending {
        batch: u64,
    },
    /// The hub took it at `sent`; the device has not reported it yet.
    Waiting {
        sent: Timestamp,
    },
    /// Not reported in time: the device is being read back.
    Checking {
        batch: u64,
    },
}

/// A change from here that has not shown yet.
#[derive(Debug, Clone)]
pub struct Pending {
    pub device: DeviceId,
    pub kind: Kind,
    pub wanted: Change,
    pub stage: Stage,
    /// The activity line that tells how it goes.
    entry: u64,
    asked: Timestamp,
    /// The device as it was, for the "before" of the activity line.
    before: Box<Device>,
    /// What the device has reported since the change went out.
    reported: Attributes,
    /// A newer value came while this one was being sent.
    again: bool,
    /// An earlier value went out, and the device may still report it.
    went_out: bool,
}

/// What `u` takes back.
#[derive(Debug, Clone, PartialEq)]
pub enum Undo {
    /// The changes that put devices back as they were.
    Changes {
        label: &'static str,
        restore: Vec<(DeviceId, Vec<Change>)>,
        at: Timestamp,
    },
    /// A scene's latest run, while the hub allows undoing it.
    Scene {
        id: SceneId,
        name: String,
        until: Timestamp,
    },
}

/// A brightness bar running from one value to another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    from: f64,
    to: f64,
    start: Timestamp,
}

impl Motion {
    pub fn running(&self, now: Timestamp) -> bool {
        now.duration_since(self.start) < BAR_MOTION
    }

    /// Where the bar is at `now`, easing out towards the new value.
    pub fn value(&self, now: Timestamp) -> f64 {
        let elapsed = now.duration_since(self.start).as_secs_f64();
        let t = (elapsed / BAR_MOTION.as_secs_f64()).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from + (self.to - self.from) * eased
    }
}

/// What the mouse wheel turns, over the bars of the selected light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wheel {
    Brightness,
    White,
    Hue,
    Saturation,
}

/// A row of the color picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerRow {
    Presets,
    Hue,
    Saturation,
    White,
}

/// The color picker for one light.
#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    pub device: DeviceId,
    pub row: PickerRow,
    /// The preset `←→` is on in the presets row.
    pub preset: usize,
    /// What Esc puts back.
    backup: Vec<Change>,
}

impl Picker {
    /// The rows the light has, in order.
    pub fn rows(device: &Device) -> Vec<PickerRow> {
        let controls = device.controls();
        let mut rows = vec![PickerRow::Presets];
        if controls.color {
            rows.extend([PickerRow::Hue, PickerRow::Saturation]);
        }
        if controls.white.is_some() {
            rows.push(PickerRow::White);
        }
        rows
    }
}

/// Named whites for the light's range, then the named colors, for what the light can show.
pub fn presets(device: &Device) -> Vec<(&'static str, Change)> {
    let controls = device.controls();
    let mut presets = Vec::new();
    if let Some(range) = controls.white {
        for (name, white) in [
            ("warm", White::Warm),
            ("neutral", White::Neutral),
            ("cool", White::Cool),
        ] {
            presets.push((name, Change::White(white.kelvin(range))));
        }
    }
    if controls.color {
        presets.extend(PRESETS.iter().map(|(name, color)| {
            (
                *name,
                Change::Color {
                    hue: color.hue,
                    saturation: color.saturation,
                },
            )
        }));
    }
    presets
}

/// The changes that put `device` back as it is, for the kinds `changes` touch. A light that was
/// off is switched off last, one that was on is switched on first.
fn restore(device: &Device, changes: &[Change]) -> Vec<Change> {
    let a = &device.attributes;
    let mut back = Vec::new();
    for kind in [Kind::Brightness, Kind::Shade] {
        if !changes.iter().any(|change| self::kind(change) == kind) {
            continue;
        }
        match kind {
            Kind::Brightness => back.extend(a.light_level().map(Change::Brightness)),
            _ => match (
                a.color_mode(),
                a.color_hue(),
                a.color_saturation(),
                a.color_temperature(),
            ) {
                (Some(ColorMode::Color), Some(hue), Some(saturation), _) => {
                    back.push(Change::Color { hue, saturation });
                }
                (_, _, _, Some(kelvin)) => back.push(Change::White(kelvin)),
                _ => {}
            },
        }
    }
    if changes.iter().any(|change| kind(change) == Kind::Power) {
        match a.is_on() {
            Some(true) => back.insert(0, Change::Power(true)),
            _ => back.push(Change::Power(false)),
        }
    }
    back
}

/// The step `[` and `]` take: a tenth of the light's range, in tens of kelvin.
fn white_step(range: TemperatureRange) -> u32 {
    ((range.coolest - range.warmest) / 100 * 10).max(FINE_KELVIN)
}

impl Model {
    /// What a tick does for changes: send what is due, read back what did not show, and let go
    /// of finished flashes.
    pub(super) fn tick_changes(&mut self) -> Vec<Effect> {
        let now = self.now;
        self.flashes
            .retain(|_, at| now.duration_since(*at) < super::FLASH);
        self.switched
            .retain(|_, at| now.duration_since(*at) < super::SWITCH_FLASH);
        self.motions.retain(|_, motion| motion.running(now));
        self.identifying.retain(|_, until| *until > now);
        self.scene_runs.retain(|_, until| *until > now);
        let mut effects = self.dispatch();
        effects.extend(self.overdue());
        effects
    }

    /// The home as the screen shows it: with the changes from here that are on their way.
    pub fn shown_home(&self) -> Option<Home> {
        let mut home = self.home.clone()?;
        for device in &mut home.devices {
            *device = self.shown(device).into_owned();
        }
        Some(home)
    }

    /// Whether changes can go out now; says why not otherwise.
    fn can_send(&mut self) -> bool {
        if self.client.is_some() && self.link == Link::Live {
            return true;
        }
        self.say(Tone::Bad, "offline: nothing is sent until the hub is back");
        false
    }

    pub(super) fn flash(&mut self, device: &DeviceId) {
        if self.animations {
            self.flashes.insert(device.clone(), self.now);
        }
    }

    pub(super) fn run_bar(&mut self, device: &DeviceId, from: u8, to: u8) {
        if !self.animations || from == to {
            return;
        }
        let now = self.now;
        let from = self
            .motions
            .get(device)
            .filter(|motion| motion.running(now))
            .map_or(f64::from(from), |motion| motion.value(now));
        self.motions.insert(
            device.clone(),
            Motion {
                from,
                to: f64::from(to),
                start: now,
            },
        );
    }

    /// Asks the selected device or room for `wanted`, the same way `dirigent on` and the others
    /// do.
    fn change(&mut self, wanted: Wanted, label: &'static str) -> Vec<Effect> {
        let Some(home) = self.shown_home() else {
            return vec![];
        };
        let target = match &self.selected {
            Some(Row::Device(id)) => home.device(id).map(Target::Device),
            Some(Row::Room(id)) => home.room(id).map(Target::Room),
            Some(Row::Scene(_)) | None => None,
        };
        let Some(target) = target else {
            return vec![];
        };
        let plans: Vec<(Device, Vec<Change>)> = match control::plan(&home, &[target], wanted) {
            Ok(plans) => plans
                .into_iter()
                .filter(|plan| !plan.changes.is_empty())
                .map(|plan| (plan.device.clone(), plan.changes))
                .collect(),
            Err(error) => {
                self.say(Tone::Warn, error.to_string());
                return vec![];
            }
        };
        if plans.is_empty() {
            self.say(Tone::Info, "nothing to change: it shows that already");
            return vec![];
        }
        if !self.can_send() {
            return vec![];
        }
        self.remember(label, &plans);
        for (device, changes) in plans {
            self.queue_all(device, changes);
        }
        self.dispatch()
    }

    /// Queues `changes` to `device` in order, each on top of the one before.
    fn queue_all(&mut self, mut device: Device, changes: Vec<Change>) {
        for change in changes {
            self.queue(&device, change);
            device.attributes.apply(&shows(&change));
        }
    }

    /// Puts a change on its way; `before` is the device as the screen shows it now.
    fn queue(&mut self, before: &Device, change: Change) {
        let kind = kind(&change);
        let due = self.now + COALESCE;
        if kind == Kind::Power && self.animations {
            self.switched.insert(before.id.clone(), self.now);
        }
        if let (Change::Brightness(to), Some(from)) = (change, before.attributes.light_level()) {
            self.run_bar(&before.id, from, to);
        }
        let reported = self
            .device(&before.id)
            .map(|device| device.attributes.clone());
        if let Some(index) = self
            .pending
            .iter()
            .position(|p| p.device == before.id && p.kind == kind)
        {
            let pending = &mut self.pending[index];
            pending.wanted = change;
            match pending.stage {
                Stage::Queued { .. } => {
                    // Asked back to what the device shows: nothing to send.
                    if reported.is_some_and(|reported| change.is_shown_by(&reported)) {
                        let gone = self.pending.remove(index);
                        self.activity.retain(|entry| entry.id != gone.entry);
                        return;
                    }
                }
                Stage::Sending { .. } => pending.again = true,
                Stage::Waiting { .. } | Stage::Checking { .. } => {
                    pending.stage = Stage::Queued { due };
                    pending.reported = Attributes::default();
                }
            }
            let what = control::describe(&pending.before, &change);
            let entry = pending.entry;
            if let Some(line) = self.entry(entry) {
                line.what = what;
                line.tone = Tone::Pending;
                line.source = Some("dirigent".to_owned());
            }
            return;
        }
        let what = control::describe(before, &change);
        let entry = self.log(
            Tone::Pending,
            before.name(),
            &what,
            Some("dirigent".to_owned()),
        );
        self.pending.push(Pending {
            device: before.id.clone(),
            kind,
            wanted: change,
            stage: Stage::Queued { due },
            entry,
            asked: self.now,
            before: Box::new(before.clone()),
            reported: Attributes::default(),
            again: false,
            went_out: false,
        });
    }

    /// Records how to take back what `plans` change, unless it continues the latest step.
    fn remember(&mut self, label: &'static str, plans: &[(Device, Vec<Change>)]) {
        let restore: Vec<(DeviceId, Vec<Change>)> = plans
            .iter()
            .map(|(device, changes)| (device.id.clone(), restore(device, changes)))
            .collect();
        if let Some(Undo::Changes {
            label: last,
            restore: earlier,
            at,
        }) = self.undo.last_mut()
            && *last == label
            && self.now.duration_since(*at) < UNDO_MERGE
            && earlier
                .iter()
                .map(|(id, _)| id)
                .eq(restore.iter().map(|(id, _)| id))
        {
            // A held key: the step still goes back to before the first press.
            *at = self.now;
            return;
        }
        self.undo.push(Undo::Changes {
            label,
            restore,
            at: self.now,
        });
        if self.undo.len() > UNDO_KEPT {
            self.undo.remove(0);
        }
    }

    /// Sends what is due, one batch per device, and never two batches to one device at once.
    fn dispatch(&mut self) -> Vec<Effect> {
        let Some(client) = self.client.clone() else {
            return vec![];
        };
        let now = self.now;
        let mut due: Vec<DeviceId> = Vec::new();
        for pending in &self.pending {
            if matches!(pending.stage, Stage::Queued { due: at } if at <= now)
                && !due.contains(&pending.device)
            {
                due.push(pending.device.clone());
            }
        }
        let mut effects = Vec::new();
        for id in due {
            let busy = self.pending.iter().any(|p| {
                p.device == id && matches!(p.stage, Stage::Sending { .. } | Stage::Checking { .. })
            });
            if busy {
                continue;
            }
            let Some(device) = self.device(&id).cloned() else {
                self.pending.retain(|p| p.device != id);
                continue;
            };
            let batch = self.next_id();
            let mut changes = Vec::new();
            for pending in &mut self.pending {
                if pending.device == id
                    && matches!(pending.stage, Stage::Queued { due: at } if at <= now)
                {
                    pending.stage = Stage::Sending { batch };
                    pending.went_out = true;
                    changes.push(pending.wanted);
                }
            }
            // On before anything else, off after it.
            changes.sort_by_key(|change| match change {
                Change::Power(true) => 0,
                Change::Power(false) => 2,
                _ => 1,
            });
            effects.push(Effect::Send {
                client: client.clone(),
                device: Box::new(device),
                changes,
                batch,
            });
        }
        effects
    }

    /// Reads back the devices that did not report a change in time.
    fn overdue(&mut self) -> Vec<Effect> {
        let Some(client) = self.client.clone() else {
            return vec![];
        };
        let late = SignedDuration::try_from(CONFIRM_WITHIN).unwrap_or_default();
        let mut effects = Vec::new();
        let mut checked: Vec<DeviceId> = Vec::new();
        for index in 0..self.pending.len() {
            let pending = &self.pending[index];
            let overdue = matches!(pending.stage, Stage::Waiting { sent } if self.now.duration_since(sent) >= late);
            if !overdue || checked.contains(&pending.device) {
                continue;
            }
            let id = pending.device.clone();
            let batch = self.next_id();
            for pending in &mut self.pending {
                if pending.device == id && matches!(pending.stage, Stage::Waiting { .. }) {
                    pending.stage = Stage::Checking { batch };
                }
            }
            effects.push(Effect::ReadBack {
                client: client.clone(),
                device: id.clone(),
                batch,
            });
            checked.push(id);
        }
        effects
    }

    pub(super) fn sent(&mut self, batch: u64, result: Result<(), Failure>) -> Vec<Effect> {
        match result {
            Ok(()) => {
                let now = self.now;
                let mut done = Vec::new();
                for (index, pending) in self.pending.iter_mut().enumerate() {
                    if pending.stage != (Stage::Sending { batch }) {
                        continue;
                    }
                    if pending.again {
                        pending.again = false;
                        pending.stage = Stage::Queued { due: now };
                    } else {
                        pending.stage = Stage::Waiting { sent: now };
                        if pending.wanted.is_shown_by(&pending.reported) {
                            done.push(index);
                        }
                    }
                }
                for index in done.into_iter().rev() {
                    self.confirm(index);
                }
            }
            Err(failure) => {
                let failed: Vec<Pending> = self.take(|p| p.stage == Stage::Sending { batch });
                for pending in &failed {
                    if let Some(line) = self.entry(pending.entry) {
                        line.tone = Tone::Bad;
                        line.source = Some("failed".to_owned());
                    }
                }
                if failure.kind != FailureKind::Other {
                    return self.failed(failure);
                }
                self.say(Tone::Bad, failure.message);
            }
        }
        self.dispatch()
    }

    /// Removes and returns the pending changes that `which` picks.
    fn take(&mut self, which: impl Fn(&Pending) -> bool) -> Vec<Pending> {
        let (taken, kept) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| which(p));
        self.pending = kept;
        taken
    }

    fn confirm(&mut self, index: usize) {
        let pending = self.pending.remove(index);
        let seconds = self.now.duration_since(pending.asked).as_secs_f64();
        if let Some(line) = self.entry(pending.entry) {
            line.tone = Tone::Good;
            line.source = Some(format!("{seconds:.1} s"));
        }
    }

    /// Takes in a state change for changes from here. Returns whether it answers one of them;
    /// otherwise it came from elsewhere.
    pub(super) fn observe(&mut self, update: &DeviceUpdate, _at: Timestamp) -> bool {
        let mut answers = false;
        let mut done = Vec::new();
        for (index, pending) in self.pending.iter_mut().enumerate() {
            if pending.device != update.id {
                continue;
            }
            let reports = update
                .attributes
                .iter()
                .any(|(key, _)| reported_as(pending.kind).contains(&key));
            if matches!(pending.stage, Stage::Queued { .. }) {
                // The device may still report a value that went out before this one.
                answers |= reports && pending.went_out;
                continue;
            }
            answers |= reports;
            pending.reported.apply(&update.attributes);
            let reported = pending.wanted.is_shown_by(&pending.reported);
            // A change being sent is confirmed once the hub took it.
            if reported
                && matches!(
                    pending.stage,
                    Stage::Waiting { .. } | Stage::Checking { .. }
                )
            {
                done.push(index);
            }
        }
        for index in done.into_iter().rev() {
            self.confirm(index);
        }
        answers
    }

    pub(super) fn read_back(
        &mut self,
        batch: u64,
        result: Result<Box<Device>, Failure>,
    ) -> Vec<Effect> {
        let checked = self.take(|p| p.stage == Stage::Checking { batch });
        match result {
            Ok(device) => {
                for pending in &checked {
                    let (tone, source) = if pending.wanted.is_shown_by(&device.attributes) {
                        (Tone::Warn, "the device did not report it".to_owned())
                    } else {
                        (
                            Tone::Bad,
                            format!("the hub reports {}", theme::describe_state(&device)),
                        )
                    };
                    if let Some(line) = self.entry(pending.entry) {
                        line.tone = tone;
                        line.source = Some(source);
                    }
                }
                if let Some(home) = &mut self.home
                    && let Some(known) = home.devices.iter_mut().find(|d| d.id == device.id)
                {
                    *known = *device;
                }
            }
            Err(failure) => {
                for pending in &checked {
                    if let Some(line) = self.entry(pending.entry) {
                        line.tone = Tone::Bad;
                        line.source = Some("could not read it back".to_owned());
                    }
                }
                self.say(Tone::Bad, failure.message);
            }
        }
        vec![]
    }

    pub(super) fn activate(&mut self) -> Vec<Effect> {
        match self.selected.clone() {
            Some(Row::Scene(id)) => {
                if !self.can_send() {
                    return vec![];
                }
                let client = self.client.clone().expect("can_send needs a client");
                self.scene_moves.insert(id.clone(), self.now);
                vec![Effect::RunScene {
                    client,
                    scene: id,
                    undo: false,
                }]
            }
            Some(Row::Device(id))
                if self
                    .device(&id)
                    .is_some_and(|d| d.kind() == DeviceKind::Controller) =>
            {
                let name = self
                    .device(&id)
                    .map(|d| d.name().to_owned())
                    .unwrap_or_default();
                self.say(
                    Tone::Warn,
                    format!("{name} is read only: the hub cannot press it"),
                );
                vec![]
            }
            Some(_) => self.change(Wanted::Toggle, "on/off"),
            None => vec![],
        }
    }

    pub(super) fn dim(&mut self, step: i16) -> Vec<Effect> {
        if matches!(self.selected, Some(Row::Scene(_))) {
            self.say(Tone::Warn, "a scene has no brightness");
            return vec![];
        }
        self.change(Wanted::Brightness(Level::By(step)), "brightness")
    }

    /// The selected light one step warmer (`direction` -1) or cooler (1).
    pub(super) fn white_step(&mut self, direction: i32) -> Vec<Effect> {
        let Some(Row::Device(id)) = &self.selected else {
            self.say(Tone::Warn, "select a light for white");
            return vec![];
        };
        let Some(device) = self.device(id).map(|d| self.shown(d).into_owned()) else {
            return vec![];
        };
        let Some(range) = device.controls().white else {
            return self.change(Wanted::White(White::Warm), "white");
        };
        let current = device
            .attributes
            .color_temperature()
            .unwrap_or_else(|| range.warmest.midpoint(range.coolest));
        let step = white_step(range);
        let kelvin = if direction < 0 {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step)
        };
        self.change(Wanted::White(White::Kelvin(range.clamp(kelvin))), "white")
    }

    pub(super) fn identify(&mut self) -> Vec<Effect> {
        let Some(Row::Device(id)) = self.selected.clone() else {
            self.say(Tone::Warn, "identify works on a light or an outlet");
            return vec![];
        };
        if !self
            .device(&id)
            .is_some_and(|d| matches!(d.kind(), DeviceKind::Light | DeviceKind::Outlet))
        {
            self.say(Tone::Warn, "identify works on a light or an outlet");
            return vec![];
        }
        if !self.can_send() {
            return vec![];
        }
        let client = self.client.clone().expect("can_send needs a client");
        vec![Effect::Identify { client, device: id }]
    }

    pub(super) fn identified(
        &mut self,
        device: &DeviceId,
        result: Result<(), Failure>,
    ) -> Vec<Effect> {
        let name = self
            .device(device)
            .map(|d| d.name().to_owned())
            .unwrap_or_default();
        match result {
            Ok(()) => {
                let seconds = IDENTIFY.as_secs();
                self.identifying.insert(
                    device.clone(),
                    self.now + SignedDuration::try_from(IDENTIFY).unwrap_or_default(),
                );
                self.log(
                    Tone::Info,
                    &name,
                    &format!("identify · blinking {seconds} s"),
                    Some("dirigent".to_owned()),
                );
            }
            Err(failure) => self.say(Tone::Bad, failure.message),
        }
        vec![]
    }

    pub(super) fn scene_done(
        &mut self,
        scene: &SceneId,
        undo: bool,
        result: Result<(), Failure>,
    ) -> Vec<Effect> {
        let found = self.home.as_ref().and_then(|home| home.scene(scene));
        let name = found.map_or_else(|| scene.to_string(), |s| s.name().to_owned());
        let window = found.and_then(dirigera::Scene::undo_window);
        let who = format!("scene {name}");
        match result {
            Ok(()) if undo => {
                self.scene_runs.remove(scene);
                self.log(Tone::Good, &who, "undone", Some("dirigent".to_owned()));
            }
            Ok(()) => {
                self.log(Tone::Good, &who, "ran", Some("dirigent".to_owned()));
                if let Some(window) = window {
                    let until = self.now + SignedDuration::try_from(window).unwrap_or_default();
                    self.scene_runs.insert(scene.clone(), until);
                    self.undo.push(Undo::Scene {
                        id: scene.clone(),
                        name,
                        until,
                    });
                }
            }
            Err(failure) => {
                self.scene_moves.remove(scene);
                self.log(
                    Tone::Bad,
                    &who,
                    if undo {
                        "could not be undone"
                    } else {
                        "did not run"
                    },
                    None,
                );
                self.say(Tone::Bad, failure.message);
            }
        }
        vec![]
    }

    /// `u`: takes back the latest step.
    pub(super) fn take_back(&mut self) -> Vec<Effect> {
        let Some(step) = self.undo.pop() else {
            self.say(Tone::Info, "nothing to undo");
            return vec![];
        };
        if !self.can_send() {
            self.undo.push(step);
            return vec![];
        }
        let client = self.client.clone().expect("can_send needs a client");
        match step {
            Undo::Scene { id, name, until } => {
                if self.now < until {
                    self.scene_moves.insert(id.clone(), self.now);
                    return vec![Effect::RunScene {
                        client,
                        scene: id,
                        undo: true,
                    }];
                }
                self.say(
                    Tone::Warn,
                    format!("the undo window of scene {name} has ended"),
                );
                vec![]
            }
            Undo::Changes { label, restore, .. } => {
                for (id, changes) in restore {
                    let Some(device) = self.device(&id).map(|d| self.shown(d).into_owned()) else {
                        continue;
                    };
                    let changes: Vec<Change> = changes
                        .into_iter()
                        .filter(|change| change.check(&device).is_ok())
                        .collect();
                    self.queue_all(device, changes);
                }
                self.say(Tone::Info, format!("took back {label}"));
                self.dispatch()
            }
        }
    }

    pub(super) fn wheel(&mut self, wheel: Wheel, up: bool) -> Vec<Effect> {
        if self.picker.is_some() || self.overlay != super::Overlay::None {
            return vec![];
        }
        let direction = if up { 1 } else { -1 };
        match wheel {
            Wheel::Brightness => self.dim(direction * WHEEL_BRIGHTNESS),
            Wheel::White => self.white_step(i32::from(direction)),
            Wheel::Hue | Wheel::Saturation => {
                let Some(Row::Device(id)) = &self.selected else {
                    return vec![];
                };
                let Some(device) = self.device(id).map(|d| self.shown(d).into_owned()) else {
                    return vec![];
                };
                let change = color_step(&device, wheel == Wheel::Hue, f64::from(direction), false);
                self.change(color_wanted(change), "color")
            }
        }
    }

    pub(super) fn open_picker(&mut self) {
        let Some(Row::Device(id)) = &self.selected else {
            self.say(Tone::Warn, "select a light to pick its color");
            return;
        };
        let Some(device) = self.device(id).map(|d| self.shown(d).into_owned()) else {
            return;
        };
        let controls = device.controls();
        if !controls.color && controls.white.is_none() {
            self.say(Tone::Warn, format!("{} shows no colors", device.name()));
            return;
        }
        let presets = presets(&device);
        let a = &device.attributes;
        let distance = |change: &Change| match (change, a.color_mode()) {
            (Change::White(kelvin), Some(ColorMode::Temperature)) => {
                f64::from(kelvin.abs_diff(a.color_temperature().unwrap_or(0)))
            }
            (Change::Color { hue, .. }, Some(ColorMode::Color)) => {
                let off = (hue - a.color_hue().unwrap_or(0.0)).rem_euclid(360.0);
                off.min(360.0 - off) * 10.0
            }
            _ => f64::MAX,
        };
        let preset = (0..presets.len())
            .min_by(|&x, &y| distance(&presets[x].1).total_cmp(&distance(&presets[y].1)))
            .unwrap_or(0);
        self.picker = Some(Picker {
            device: device.id.clone(),
            row: PickerRow::Presets,
            preset,
            backup: restore(&device, &[Change::White(0)]),
        });
    }

    pub(super) fn picker_key(&mut self, action: Action) -> Vec<Effect> {
        let Some(picker) = self.picker.clone() else {
            return vec![];
        };
        let Some(device) = self
            .device(&picker.device)
            .map(|d| self.shown(d).into_owned())
        else {
            self.picker = None;
            return vec![];
        };
        match action {
            Action::Up | Action::Down => {
                let rows = Picker::rows(&device);
                let at = rows.iter().position(|row| *row == picker.row).unwrap_or(0);
                let next = if action == Action::Up {
                    (at + rows.len() - 1) % rows.len()
                } else {
                    (at + 1) % rows.len()
                };
                if let Some(open) = &mut self.picker {
                    open.row = rows[next];
                }
                vec![]
            }
            Action::Brighter { fine } | Action::Dimmer { fine } => {
                let direction = if matches!(action, Action::Brighter { .. }) {
                    1.0
                } else {
                    -1.0
                };
                let change = match picker.row {
                    PickerRow::Presets => {
                        let presets = presets(&device);
                        let count = presets.len();
                        let next = if direction > 0.0 {
                            (picker.preset + 1) % count
                        } else {
                            (picker.preset + count - 1) % count
                        };
                        if let Some(open) = &mut self.picker {
                            open.preset = next;
                        }
                        presets[next].1
                    }
                    PickerRow::Hue => color_step(&device, true, direction, fine),
                    PickerRow::Saturation => color_step(&device, false, direction, fine),
                    PickerRow::White => {
                        let Some(range) = device.controls().white else {
                            return vec![];
                        };
                        let current = device
                            .attributes
                            .color_temperature()
                            .unwrap_or(range.warmest);
                        let step = if fine { FINE_KELVIN } else { white_step(range) };
                        let kelvin = if direction > 0.0 {
                            current + step
                        } else {
                            current.saturating_sub(step)
                        };
                        Change::White(range.clamp(kelvin))
                    }
                };
                if !self.can_send() {
                    return vec![];
                }
                self.queue(&device, change);
                self.dispatch()
            }
            Action::Activate => {
                self.picker = None;
                self.undo.push(Undo::Changes {
                    label: "color",
                    restore: vec![(picker.device, picker.backup)],
                    at: self.now,
                });
                vec![]
            }
            Action::Back => {
                self.picker = None;
                let back: Vec<Change> = picker
                    .backup
                    .into_iter()
                    .filter(|change| !change.is_shown_by(&device.attributes))
                    .collect();
                if back.is_empty() || !self.can_send() {
                    return vec![];
                }
                self.queue_all(device, back);
                self.dispatch()
            }
            _ => vec![],
        }
    }
}

/// The light's color one step around the hue circle, or one step more or less saturated.
fn color_step(device: &Device, hue: bool, direction: f64, fine: bool) -> Change {
    let a = &device.attributes;
    let colored = a.color_mode() == Some(ColorMode::Color);
    let current_hue = a.color_hue().unwrap_or(0.0);
    let current_saturation = if colored {
        a.color_saturation().unwrap_or(1.0)
    } else {
        1.0
    };
    let scale = if fine { 0.2 } else { 1.0 };
    if hue {
        Change::Color {
            hue: (current_hue.round() + direction * HUE_STEP * scale).rem_euclid(360.0),
            saturation: current_saturation,
        }
    } else {
        let saturation = current_saturation + direction * SATURATION_STEP * scale;
        Change::Color {
            hue: current_hue,
            saturation: (saturation * 100.0).round().clamp(0.0, 100.0) / 100.0,
        }
    }
}

fn color_wanted(change: Change) -> Wanted {
    match change {
        Change::Color { hue, saturation } => {
            Wanted::Color(crate::wanted::Color { hue, saturation })
        }
        Change::White(kelvin) => Wanted::White(White::Kelvin(kelvin)),
        Change::Power(on) => {
            if on {
                Wanted::On
            } else {
                Wanted::Off
            }
        }
        Change::Brightness(level) => Wanted::Brightness(Level::To(level)),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;
    use serde_json::json;

    use super::*;
    use crate::tui::model::tests::{LAMP, REMOTE, STRIP, event, key, live, now};
    use crate::tui::model::{Msg, Overlay};

    fn id(text: &str) -> DeviceId {
        DeviceId(text.to_owned())
    }

    fn at(millis: i64) -> Msg {
        Msg::Tick(now() + SignedDuration::from_millis(millis))
    }

    fn sends(effects: &[Effect]) -> Vec<(u64, Vec<Change>)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Send { changes, batch, .. } => Some((*batch, changes.clone())),
                _ => None,
            })
            .collect()
    }

    fn changed(device: &str, attributes: &serde_json::Value) -> Msg {
        event(&json!({
            "type": "deviceStateChanged",
            "data": { "id": device, "attributes": attributes },
        }))
    }

    #[test]
    fn a_change_shows_at_once_and_goes_out_after_the_coalesce_window() {
        let mut model = live();
        assert!(model.update(key(KeyCode::Char(' '))).is_empty());
        let strip = model.device(&id(STRIP)).unwrap();
        assert_eq!(model.shown(strip).attributes.is_on(), Some(false));
        assert_eq!(model.activity[0].tone, Tone::Pending);
        assert_eq!(model.activity[0].what, "on → off");
        assert_eq!(sends(&model.update(at(100))), []);
        let sent = sends(&model.update(at(130)));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, [Change::Power(false)]);
        model.update(Msg::Sent {
            batch: sent[0].0,
            result: Ok(()),
        });
        model.update(at(400));
        model.update(changed(STRIP, &json!({ "isOn": false })));
        assert!(model.pending.is_empty());
        assert_eq!(model.activity[0].tone, Tone::Good);
        assert_eq!(model.activity[0].source.as_deref(), Some("0.4 s"));
        assert_eq!(
            model.device(&id(STRIP)).unwrap().attributes.is_on(),
            Some(false)
        );
    }

    #[test]
    fn a_held_key_sends_a_few_requests_and_the_latest_value_last() {
        let mut model = live();
        for _ in 0..3 {
            model.update(key(KeyCode::Right));
        }
        let first = sends(&model.update(at(130)));
        assert_eq!(first[0].1, [Change::Brightness(90)], "60 % up three steps");
        // More presses while the first batch is on its way.
        model.update(key(KeyCode::Left));
        model.update(key(KeyCode::Left));
        assert!(
            sends(&model.update(at(260))).is_empty(),
            "one batch at a time"
        );
        let effects = model.update(Msg::Sent {
            batch: first[0].0,
            result: Ok(()),
        });
        let second = sends(&effects);
        assert_eq!(second[0].1, [Change::Brightness(70)]);
        assert_eq!(
            model
                .activity
                .iter()
                .filter(|line| line.tone == Tone::Pending)
                .count(),
            1
        );
    }

    #[test]
    fn a_fading_light_confirms_at_the_wanted_value_without_lines_from_elsewhere() {
        let mut model = live();
        model.update(key(KeyCode::Left));
        let sent = sends(&model.update(at(130)));
        assert_eq!(sent[0].1, [Change::Brightness(50)]);
        model.update(Msg::Sent {
            batch: sent[0].0,
            result: Ok(()),
        });
        let lines = model.activity.len();
        model.update(changed(STRIP, &json!({ "lightLevel": 55 })));
        assert_eq!(model.pending.len(), 1, "not yet at 50 %");
        assert_eq!(
            model.activity.len(),
            lines,
            "the fade is not a change from elsewhere"
        );
        let strip = model.device(&id(STRIP)).unwrap();
        assert_eq!(
            model.shown(strip).attributes.light_level(),
            Some(50),
            "the screen keeps the wanted value"
        );
        model.update(changed(STRIP, &json!({ "lightLevel": 50 })));
        assert!(model.pending.is_empty());
    }

    #[test]
    fn an_earlier_value_reported_while_the_next_waits_is_not_from_elsewhere() {
        let mut model = live();
        model.update(key(KeyCode::Right));
        let sent = sends(&model.update(at(130)));
        assert_eq!(sent[0].1, [Change::Brightness(70)]);
        model.update(Msg::Sent {
            batch: sent[0].0,
            result: Ok(()),
        });
        model.update(at(300));
        model.update(key(KeyCode::Right));
        let lines = model.activity.len();
        // The light fades to 70 % while 80 % waits for the coalesce window.
        model.update(changed(STRIP, &json!({ "lightLevel": 66 })));
        assert_eq!(model.activity.len(), lines, "{:?}", model.activity);
        assert!(!model.flashes.contains_key(&id(STRIP)));
        let sent = sends(&model.update(at(430)));
        assert_eq!(sent[0].1, [Change::Brightness(80)]);
    }

    #[test]
    fn a_change_that_never_shows_is_read_back() {
        let mut model = live();
        model.update(key(KeyCode::Char(' ')));
        let sent = sends(&model.update(at(130)));
        model.update(Msg::Sent {
            batch: sent[0].0,
            result: Ok(()),
        });
        assert!(model.update(at(4_000)).is_empty());
        let effects = model.update(at(5_200));
        let Some(Effect::ReadBack { batch, device, .. }) = effects.first() else {
            panic!("{effects:?}");
        };
        assert_eq!(device, &id(STRIP));
        let unchanged = model.device(&id(STRIP)).unwrap().clone();
        model.update(Msg::ReadBack {
            batch: *batch,
            result: Ok(Box::new(unchanged)),
        });
        assert!(model.pending.is_empty());
        assert_eq!(model.activity[0].tone, Tone::Bad);
        assert!(
            model.activity[0]
                .source
                .as_deref()
                .unwrap()
                .starts_with("the hub reports on")
        );
    }

    #[test]
    fn pressing_twice_before_sending_sends_nothing() {
        let mut model = live();
        model.update(key(KeyCode::Char(' ')));
        model.update(key(KeyCode::Char(' ')));
        assert!(model.pending.is_empty());
        assert_eq!(sends(&model.update(at(200))), []);
        assert!(model.activity.iter().all(|line| line.tone != Tone::Pending));
    }

    #[test]
    fn what_a_device_cannot_do_is_said_and_not_sent() {
        let mut model = live();
        model.update(Msg::Select(Row::Device(id(LAMP))));
        model.update(key(KeyCode::Right));
        assert_eq!(
            model.notice.as_ref().unwrap().text,
            "Reading lamp cannot dim"
        );
        model.update(Msg::Select(Row::Device(id(REMOTE))));
        model.update(key(KeyCode::Char(' ')));
        assert!(model.notice.as_ref().unwrap().text.contains("read only"));
        assert!(model.pending.is_empty());
    }

    #[test]
    fn nothing_goes_out_while_offline() {
        let mut model = live();
        model.update(Msg::Lost("gone".into()));
        model.update(key(KeyCode::Char(' ')));
        assert!(model.pending.is_empty());
        assert!(model.notice.as_ref().unwrap().text.starts_with("offline"));
    }

    #[test]
    fn a_room_switches_as_a_whole() {
        let mut model = live();
        model.update(Msg::Select(Row::Room(dirigera::RoomId(
            "a1b2c3d4-0000-4000-8000-0000000000a2".into(),
        ))));
        model.update(key(KeyCode::Char(' ')));
        let sent = sends(&model.update(at(130)));
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].1,
            [Change::Power(true)],
            "the reading lamp; the remote has no power"
        );
    }

    #[test]
    fn undo_takes_back_a_held_key_as_one_step() {
        let mut model = live();
        for _ in 0..3 {
            model.update(key(KeyCode::Right));
        }
        assert_eq!(model.undo.len(), 1);
        let sent = sends(&model.update(at(130)));
        model.update(Msg::Sent {
            batch: sent[0].0,
            result: Ok(()),
        });
        model.update(changed(STRIP, &json!({ "lightLevel": 90 })));
        model.update(key(KeyCode::Char('u')));
        let sent = sends(&model.update(at(300)));
        assert_eq!(sent[0].1, [Change::Brightness(60)]);
        assert_eq!(model.notice.as_ref().unwrap().text, "took back brightness");
    }

    #[test]
    fn dimming_an_off_light_switches_it_on_first_and_undo_switches_it_off_last() {
        let mut model = live();
        model.update(key(KeyCode::Char(' ')));
        let sent = sends(&model.update(at(130)));
        model.update(Msg::Sent {
            batch: sent[0].0,
            result: Ok(()),
        });
        model.update(changed(STRIP, &json!({ "isOn": false })));
        model.update(at(3_000));
        model.update(key(KeyCode::Right));
        let sent = sends(&model.update(at(3_200)));
        assert_eq!(sent[0].1, [Change::Power(true), Change::Brightness(70)]);
        model.update(Msg::Sent {
            batch: sent[0].0,
            result: Ok(()),
        });
        model.update(changed(STRIP, &json!({ "isOn": true, "lightLevel": 70 })));
        model.update(key(KeyCode::Char('u')));
        let sent = sends(&model.update(at(3_400)));
        assert_eq!(sent[0].1, [Change::Brightness(60), Change::Power(false)]);
    }

    #[test]
    fn scenes_run_and_can_be_taken_back_within_their_window() {
        let mut model = live();
        let scene = dirigera::SceneId("a1b2c3d4-0000-4000-8000-0000000000b1".into());
        model.update(Msg::Select(Row::Scene(scene.clone())));
        let effects = model.update(key(KeyCode::Enter));
        assert!(matches!(
            effects.as_slice(),
            [Effect::RunScene { undo: false, .. }]
        ));
        model.update(Msg::SceneDone {
            scene: scene.clone(),
            undo: false,
            result: Ok(()),
        });
        assert!(model.scene_runs.contains_key(&scene));
        let effects = model.update(key(KeyCode::Char('u')));
        assert!(matches!(
            effects.as_slice(),
            [Effect::RunScene { undo: true, .. }]
        ));
        model.update(Msg::SceneDone {
            scene: scene.clone(),
            undo: false,
            result: Ok(()),
        });
        model.update(at(31_000));
        assert!(model.update(key(KeyCode::Char('u'))).is_empty());
        assert!(
            model
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("undo window of scene Evening has ended")
        );
    }

    #[test]
    fn the_picker_previews_and_puts_back_on_escape() {
        let mut model = live();
        model.update(key(KeyCode::Char('c')));
        let picker = model.picker.clone().unwrap();
        // Shelf strip shows 30° at 80 %: the orange preset at 28° is closest.
        assert_eq!(
            presets(model.device(&id(STRIP)).unwrap())[picker.preset].0,
            "orange"
        );
        model.update(key(KeyCode::Right));
        let strip = model.device(&id(STRIP)).unwrap();
        assert_eq!(
            model.shown(strip).attributes.color_hue(),
            Some(50.0),
            "yellow, shown at once"
        );
        model.update(key(KeyCode::Down));
        model.update(key(KeyCode::Right));
        let strip = model.device(&id(STRIP)).unwrap();
        assert_eq!(model.shown(strip).attributes.color_hue(), Some(55.0));
        model.update(key(KeyCode::Esc));
        assert_eq!(model.picker, None);
        let strip = model.device(&id(STRIP)).unwrap();
        assert_eq!(
            model.shown(strip).attributes.color_hue(),
            Some(30.0),
            "put back"
        );
        assert_eq!(model.undo, []);
    }

    #[test]
    fn keeping_a_picked_color_can_be_undone() {
        let mut model = live();
        model.update(key(KeyCode::Char('c')));
        model.update(key(KeyCode::Left));
        model.update(key(KeyCode::Enter));
        assert_eq!(model.picker, None);
        assert_eq!(
            model.undo.last(),
            Some(&Undo::Changes {
                label: "color",
                restore: vec![(
                    id(STRIP),
                    vec![Change::Color {
                        hue: 30.0,
                        saturation: 0.8
                    }]
                )],
                at: now(),
            })
        );
    }

    #[test]
    fn identify_blinks_the_device() {
        let mut model = live();
        let effects = model.update(key(KeyCode::Char('i')));
        assert!(matches!(effects.as_slice(), [Effect::Identify { .. }]));
        model.update(Msg::Identified {
            device: id(STRIP),
            result: Ok(()),
        });
        assert!(model.identifying.contains_key(&id(STRIP)));
        assert!(model.animating());
        model.update(at(5_100));
        assert!(!model.identifying.contains_key(&id(STRIP)));
    }

    #[test]
    fn changes_from_elsewhere_flash_and_run_the_bar() {
        let mut model = live();
        model.update(changed(STRIP, &json!({ "lightLevel": 20 })));
        assert!(model.flashes.contains_key(&id(STRIP)));
        let motion = model.motions[&id(STRIP)];
        assert!((motion.value(now()) - 60.0).abs() < 1e-9);
        assert!((motion.value(now() + SignedDuration::from_millis(300)) - 20.0).abs() < 1e-9);
        assert_eq!(model.activity[0].source.as_deref(), Some("elsewhere"));
    }

    #[test]
    fn without_animations_nothing_flashes() {
        let mut model = live();
        model.animations = false;
        model.update(changed(STRIP, &json!({ "lightLevel": 20 })));
        assert!(model.flashes.is_empty() && model.motions.is_empty());
        assert!(!model.animating());
        assert_eq!(model.overlay, Overlay::None);
    }
}
