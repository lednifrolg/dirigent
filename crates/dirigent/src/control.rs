//! Commands that change devices: what to send, sending it, and waiting until it shows.
//!
//! The hub accepts a change at once; the device reports it a moment later as a
//! `deviceStateChanged` event. A light passes through other values while it fades, so a change
//! counts as confirmed when the reported state shows the wanted value, not at the first event.

use std::time::{Duration, Instant};

use dirigera::{
    Attributes, Change, ColorMode, Device, DeviceId, DeviceUpdate, EventKind, EventStream, Home,
};
use futures_util::future::join_all;
use serde::Serialize;
use tokio::time::timeout_at;

use crate::connect::Hub;
use crate::error::AppError;
use crate::names::Target;
use crate::output::{self, DIM, GREEN, RED, YELLOW, paint};
use crate::theme;
use crate::wanted::{Color, Level, White};

/// How long a device gets to report a change. Lights fade for about 2.5 s.
pub const CONFIRM_WITHIN: Duration = Duration::from_secs(5);
/// Without the event stream, how often the state is read back while waiting.
const READ_BACK_EVERY: Duration = Duration::from_millis(500);

/// What a command asks of its devices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wanted {
    On,
    Off,
    Toggle,
    Brightness(Level),
    White(White),
    Color(Color),
}

impl Wanted {
    /// What a device in a room must be able to do to take part.
    fn fits(self, device: &Device) -> bool {
        let controls = device.controls();
        match self {
            Self::On | Self::Off | Self::Toggle => controls.power,
            Self::Brightness(_) => controls.brightness,
            Self::White(_) => controls.white.is_some(),
            Self::Color(_) => controls.color,
        }
    }

    /// The devices a room needs for this, to say what it lacks.
    fn devices(self) -> &'static str {
        match self {
            Self::On | Self::Off | Self::Toggle => "devices that switch on and off",
            Self::Brightness(_) => "lights that dim",
            Self::White(_) => "lights that show white",
            Self::Color(_) => "lights that show colors",
        }
    }

    /// A change that stands for this one, to check that a device can take it at all.
    fn sample(self) -> Change {
        match self {
            Self::On | Self::Off | Self::Toggle => Change::Power(true),
            Self::Brightness(_) => Change::Brightness(50),
            // Any value: a light without white fails before its range is looked at.
            Self::White(_) => Change::White(0),
            Self::Color(_) => Change::Color {
                hue: 0.0,
                saturation: 0.0,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, clap::Args)]
pub struct Options {
    /// Return once the hub took the change, without waiting until the device shows it.
    #[arg(long)]
    pub no_wait: bool,
}

/// One device and what to send it, in order. No changes: it shows what was asked already.
struct Plan<'a> {
    device: &'a Device,
    changes: Vec<Change>,
}

fn plan<'a>(
    home: &'a Home,
    targets: &[Target<'a>],
    wanted: Wanted,
) -> Result<Vec<Plan<'a>>, AppError> {
    let mut plans: Vec<Plan<'a>> = Vec::new();
    for target in targets {
        let (devices, wanted) = match *target {
            Target::Device(device) => {
                if !matches!(wanted, Wanted::White(_)) || device.controls().white.is_none() {
                    wanted.sample().check(device)?;
                }
                (vec![device], wanted)
            }
            Target::Room(room) => {
                let devices: Vec<&Device> = home
                    .devices_in(&room.id)
                    .filter(|d| wanted.fits(d))
                    .collect();
                if devices.is_empty() {
                    return Err(AppError::Usage(format!(
                        "{} has no {}",
                        room.name,
                        wanted.devices()
                    )));
                }
                // A room toggles as a whole: off when anything in it is on.
                let wanted = match wanted {
                    Wanted::Toggle
                        if devices.iter().any(|d| d.attributes.is_on() == Some(true)) =>
                    {
                        Wanted::Off
                    }
                    Wanted::Toggle => Wanted::On,
                    other => other,
                };
                (devices, wanted)
            }
            Target::Scene(scene) => {
                return Err(AppError::Usage(format!(
                    "{} is a scene; run it with `dirigent scene`",
                    scene.name()
                )));
            }
        };
        for device in devices {
            if plans.iter().any(|plan| plan.device.id == device.id) {
                continue;
            }
            let changes = changes(device, wanted);
            for change in &changes {
                change.check(device)?;
            }
            plans.push(Plan { device, changes });
        }
    }
    Ok(plans)
}

/// What to send `device` so it shows `wanted`: nothing if it does already. Dimming, white and
/// color switch an off light on first.
fn changes(device: &Device, wanted: Wanted) -> Vec<Change> {
    let a = &device.attributes;
    let on = a.is_on() == Some(true);
    let lit = |change: Change| {
        if on {
            vec![change]
        } else {
            vec![Change::Power(true), change]
        }
    };
    match wanted {
        Wanted::On if on => vec![],
        Wanted::On => vec![Change::Power(true)],
        Wanted::Off if a.is_on() == Some(false) => vec![],
        Wanted::Off => vec![Change::Power(false)],
        Wanted::Toggle => vec![Change::Power(!on)],
        Wanted::Brightness(level) => {
            let current = a.light_level().unwrap_or(100);
            let target = level.from(current);
            match (on, target == current) {
                (true, true) => vec![],
                (false, true) => vec![Change::Power(true)],
                _ => lit(Change::Brightness(target)),
            }
        }
        Wanted::White(white) => {
            let Some(range) = device.controls().white else {
                return vec![];
            };
            let change = Change::White(white.kelvin(range));
            let shown = a.color_mode() == Some(ColorMode::Temperature) && change.is_shown_by(a);
            if on && shown { vec![] } else { lit(change) }
        }
        Wanted::Color(color) => {
            let change = Change::Color {
                hue: color.hue,
                saturation: color.saturation,
            };
            let shown = a.color_mode() == Some(ColorMode::Color) && change.is_shown_by(a);
            if on && shown { vec![] } else { lit(change) }
        }
    }
}

/// A device waiting for its changes to show.
struct Pending {
    id: DeviceId,
    changes: Vec<Change>,
    /// The device's attributes with every reported change applied.
    shown: Attributes,
    confirmed: Option<Duration>,
}

impl Pending {
    fn new(device: &Device, changes: &[Change]) -> Self {
        Self {
            id: device.id.clone(),
            changes: changes.to_vec(),
            shown: device.attributes.clone(),
            confirmed: None,
        }
    }

    fn shows_all(&self, attributes: &Attributes) -> bool {
        self.changes
            .iter()
            .all(|change| change.is_shown_by(attributes))
    }

    /// Takes in a reported state change, `at` after sending.
    fn observe(&mut self, update: &DeviceUpdate, at: Duration) {
        if update.id != self.id || self.confirmed.is_some() {
            return;
        }
        self.shown.apply(&update.attributes);
        if self.shows_all(&self.shown) {
            self.confirmed = Some(at);
        }
    }
}

/// How a device ended up.
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    Unchanged,
    /// Sent with `--no-wait`.
    Sent,
    Confirmed(Duration),
    /// The hub shows the change, but the device did not report it in time.
    HubOnly,
    /// The hub reports something else.
    Different(String),
    Failed(String),
}

#[derive(Serialize)]
struct Report {
    name: String,
    id: String,
    changes: Vec<String>,
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

pub async fn run(
    hub: &Hub,
    targets: &[Target<'_>],
    wanted: Wanted,
    options: Options,
    json: bool,
) -> Result<(), AppError> {
    let plans = plan(&hub.home, targets, wanted)?;
    let mut outcomes: Vec<Outcome> = plans.iter().map(|_| Outcome::Unchanged).collect();
    let sending: Vec<usize> = (0..plans.len())
        .filter(|&i| !plans[i].changes.is_empty())
        .collect();
    let mut first_failure = None;
    if !sending.is_empty() {
        let mut events = None;
        if !options.no_wait {
            // Opened before sending, so no report is missed.
            match EventStream::connect(&hub.client).await {
                Ok(stream) => events = Some(stream),
                Err(
                    error @ (dirigera::Error::Unauthorized
                    | dirigera::Error::CertificateChanged { .. }),
                ) => {
                    return Err(error.into());
                }
                Err(error) => output::note(format_args!(
                    "could not follow the hub's events ({error}); reading the result back instead"
                )),
            }
        }
        let started = Instant::now();
        // Devices in parallel; each device's changes in order, since some cannot be combined.
        let sends = sending.iter().map(|&i| {
            let plan = &plans[i];
            async move {
                for change in &plan.changes {
                    hub.client.apply(plan.device, change).await?;
                }
                Ok::<(), dirigera::Error>(())
            }
        });
        let results = join_all(sends).await;
        let mut pending = Vec::new();
        for (&i, result) in sending.iter().zip(results) {
            match result {
                Ok(()) if options.no_wait => outcomes[i] = Outcome::Sent,
                Ok(()) => pending.push((i, Pending::new(plans[i].device, &plans[i].changes))),
                Err(error) => {
                    let error = AppError::from(error);
                    outcomes[i] = Outcome::Failed(output::message(&error));
                    first_failure.get_or_insert(error);
                }
            }
        }
        if !pending.is_empty() {
            let deadline = tokio::time::Instant::now() + CONFIRM_WITHIN;
            match events {
                Some(stream) => wait_for_events(stream, &mut pending, started, deadline).await,
                None => read_back_until(hub, &mut pending, started, deadline).await,
            }
            for (i, waiting) in pending {
                outcomes[i] = match waiting.confirmed {
                    Some(after) => Outcome::Confirmed(after),
                    None => settle(hub, plans[i].device, &waiting).await,
                };
            }
        }
    }
    report(&plans, &outcomes, json)?;
    if let Some(error) = first_failure {
        return Err(AppError::Shown(Box::new(error)));
    }
    let unconfirmed = outcomes
        .iter()
        .filter(|o| matches!(o, Outcome::HubOnly | Outcome::Different(_)))
        .count();
    if unconfirmed > 0 {
        return Err(AppError::NotConfirmed(format!(
            "{unconfirmed} of {} changes did not show within {} s",
            sending_count(&outcomes),
            CONFIRM_WITHIN.as_secs()
        )));
    }
    Ok(())
}

fn sending_count(outcomes: &[Outcome]) -> usize {
    outcomes
        .iter()
        .filter(|o| **o != Outcome::Unchanged)
        .count()
}

async fn wait_for_events(
    mut stream: EventStream,
    pending: &mut [(usize, Pending)],
    started: Instant,
    deadline: tokio::time::Instant,
) {
    while pending.iter().any(|(_, p)| p.confirmed.is_none()) {
        let Ok(Ok(Some(event))) = timeout_at(deadline, stream.next()).await else {
            break;
        };
        if let EventKind::DeviceStateChanged(update) = &event.kind {
            for (_, waiting) in pending.iter_mut() {
                waiting.observe(update, started.elapsed());
            }
        }
    }
    stream.close().await;
}

/// Without events: reads the devices back until they show the changes.
async fn read_back_until(
    hub: &Hub,
    pending: &mut [(usize, Pending)],
    started: Instant,
    deadline: tokio::time::Instant,
) {
    loop {
        for (_, waiting) in pending.iter_mut().filter(|(_, p)| p.confirmed.is_none()) {
            if let Ok(device) = hub.client.device(&waiting.id).await
                && waiting.shows_all(&device.attributes)
            {
                waiting.confirmed = Some(started.elapsed());
            }
        }
        if pending.iter().all(|(_, p)| p.confirmed.is_some())
            || tokio::time::Instant::now() + READ_BACK_EVERY > deadline
        {
            return;
        }
        tokio::time::sleep(READ_BACK_EVERY).await;
    }
}

/// Why a device did not report a change: the hub shows it, or something else.
async fn settle(hub: &Hub, device: &Device, waiting: &Pending) -> Outcome {
    match hub.client.device(&device.id).await {
        Ok(now) if waiting.shows_all(&now.attributes) => Outcome::HubOnly,
        Ok(now) => Outcome::Different(theme::describe_state(&now)),
        Err(error) => Outcome::Different(format!(
            "could not read it back: {}",
            output::message(&error.into())
        )),
    }
}

/// "off → on", "brightness 100 → 40 %", "white 2638 → 2700 K", "color 290° 42 % → 30° 80 %".
fn describe(device: &Device, change: &Change) -> String {
    let a = &device.attributes;
    match *change {
        Change::Power(on) => {
            let was = if a.is_on() == Some(true) { "on" } else { "off" };
            format!("{was} → {}", if on { "on" } else { "off" })
        }
        Change::Brightness(level) => match a.light_level() {
            Some(was) => format!("brightness {was} → {level} %"),
            None => format!("brightness {level} %"),
        },
        Change::White(kelvin) => match (a.color_mode(), a.color_temperature()) {
            (Some(ColorMode::Temperature), Some(was)) => format!("white {was} → {kelvin} K"),
            _ => format!("white {kelvin} K"),
        },
        Change::Color { hue, saturation } => {
            let new = Color { hue, saturation };
            match (a.color_mode(), a.color_hue(), a.color_saturation()) {
                (Some(ColorMode::Color), Some(hue), Some(saturation)) => {
                    format!("color {} → {new}", Color { hue, saturation })
                }
                _ => format!("color {new}"),
            }
        }
    }
}

fn already(device: &Device) -> String {
    let state = theme::describe_state(device);
    if state.is_empty() {
        "already so".to_owned()
    } else {
        format!("already {state}")
    }
}

fn report(plans: &[Plan<'_>], outcomes: &[Outcome], json: bool) -> Result<(), AppError> {
    if json {
        let reports: Vec<Report> = plans
            .iter()
            .zip(outcomes)
            .map(|(plan, outcome)| Report {
                name: plan.device.name().to_owned(),
                id: plan.device.id.to_string(),
                changes: plan.changes.iter().map(ToString::to_string).collect(),
                result: match outcome {
                    Outcome::Unchanged => "unchanged",
                    Outcome::Sent => "sent",
                    Outcome::Confirmed(_) => "confirmed",
                    Outcome::HubOnly => "not_reported",
                    Outcome::Different(_) => "different",
                    Outcome::Failed(_) => "failed",
                },
                seconds: match outcome {
                    Outcome::Confirmed(after) => Some((after.as_secs_f64() * 10.0).round() / 10.0),
                    _ => None,
                },
                detail: match outcome {
                    Outcome::Different(what) | Outcome::Failed(what) => Some(what.clone()),
                    _ => None,
                },
            })
            .collect();
        return output::json(&reports);
    }
    let width = plans
        .iter()
        .map(|p| p.device.name().chars().count())
        .max()
        .unwrap_or(0);
    for (plan, outcome) in plans.iter().zip(outcomes) {
        let name = output::pad(plan.device.name(), width);
        let what: Vec<String> = plan
            .changes
            .iter()
            .map(|change| describe(plan.device, change))
            .collect();
        let what = what.join(", ");
        let line = match outcome {
            Outcome::Unchanged => paint(DIM, format!("· {name}  {}", already(plan.device))),
            Outcome::Sent => format!(
                "{} {name}  {what}  {}",
                paint(GREEN.bold(), "✓"),
                paint(DIM, "sent")
            ),
            Outcome::Confirmed(after) => format!(
                "{} {name}  {what}  {}",
                paint(GREEN.bold(), "✓"),
                paint(DIM, format!("{:.1} s", after.as_secs_f64()))
            ),
            Outcome::HubOnly => format!(
                "{} {name}  {what}  {}",
                paint(YELLOW.bold(), "⚠"),
                paint(
                    YELLOW,
                    format!(
                        "the hub shows it, but the device did not report it within {} s",
                        CONFIRM_WITHIN.as_secs()
                    )
                )
            ),
            Outcome::Different(now) => format!(
                "{} {name}  {what}  {}",
                paint(RED.bold(), "✗"),
                paint(RED, format!("the hub reports {now}"))
            ),
            Outcome::Failed(why) => format!(
                "{} {name}  {what}  {}",
                paint(RED.bold(), "✗"),
                paint(RED, why)
            ),
        };
        output::line(line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use dirigera::ChangeError;
    use serde_json::{Value, json};

    use super::*;

    fn device(value: Value) -> Device {
        serde_json::from_value(value).unwrap()
    }

    fn strip(attributes: &Value) -> Device {
        let mut all = json!({
            "customName": "Strip",
            "colorTemperatureMin": 4000,
            "colorTemperatureMax": 2202,
        });
        all.as_object_mut()
            .unwrap()
            .extend(attributes.as_object().unwrap().clone());
        device(json!({
            "id": "strip_1",
            "deviceType": "light",
            "attributes": all,
            "capabilities": {
                "canReceive": ["isOn", "lightLevel", "colorTemperature", "colorHue", "colorSaturation"],
            },
        }))
    }

    fn update(attributes: &Value) -> DeviceUpdate {
        serde_json::from_value(json!({ "id": "strip_1", "attributes": attributes })).unwrap()
    }

    #[test]
    fn nothing_is_sent_for_what_shows_already() {
        let lit = strip(
            &json!({"isOn": true, "lightLevel": 40, "colorMode": "temperature", "colorTemperature": 2702}),
        );
        assert_eq!(changes(&lit, Wanted::On), []);
        assert_eq!(changes(&lit, Wanted::Brightness(Level::To(40))), []);
        assert_eq!(changes(&lit, Wanted::White(White::Kelvin(2700))), []);
        assert_eq!(changes(&lit, Wanted::Off), [Change::Power(false)]);
        assert_eq!(changes(&lit, Wanted::Toggle), [Change::Power(false)]);
        assert_eq!(
            changes(&lit, Wanted::Brightness(Level::By(-10))),
            [Change::Brightness(30)]
        );
    }

    #[test]
    fn dimming_or_coloring_an_off_light_switches_it_on_first() {
        let dark = strip(&json!({"isOn": false, "lightLevel": 40}));
        assert_eq!(
            changes(&dark, Wanted::Brightness(Level::To(40))),
            [Change::Power(true)]
        );
        assert_eq!(
            changes(&dark, Wanted::Brightness(Level::To(60))),
            [Change::Power(true), Change::Brightness(60)]
        );
        assert_eq!(
            changes(&dark, Wanted::White(White::Warm)),
            [Change::Power(true), Change::White(2202)]
        );
        assert_eq!(
            changes(
                &dark,
                Wanted::Color(Color {
                    hue: 30.0,
                    saturation: 0.8
                })
            ),
            [
                Change::Power(true),
                Change::Color {
                    hue: 30.0,
                    saturation: 0.8
                }
            ]
        );
    }

    #[test]
    fn fading_lights_confirm_at_the_final_value() {
        // Seen live: dimming from 100 to 40 reported 93 first, 40 about 2.4 s after the send.
        let light = strip(&json!({"isOn": true, "lightLevel": 100}));
        let mut waiting = Pending::new(&light, &[Change::Brightness(40)]);
        waiting.observe(
            &update(&json!({"lightLevel": 93})),
            Duration::from_millis(400),
        );
        assert_eq!(waiting.confirmed, None);
        waiting.observe(
            &update(&json!({"lightLevel": 40})),
            Duration::from_millis(2400),
        );
        assert_eq!(waiting.confirmed, Some(Duration::from_millis(2400)));
        // Later reports do not move the confirmation.
        waiting.observe(
            &update(&json!({"lightLevel": 1})),
            Duration::from_millis(4000),
        );
        assert_eq!(waiting.confirmed, Some(Duration::from_millis(2400)));
    }

    #[test]
    fn every_change_of_a_device_has_to_show() {
        let dark = strip(&json!({"isOn": false, "lightLevel": 100}));
        let mut waiting = Pending::new(&dark, &[Change::Power(true), Change::Brightness(60)]);
        waiting.observe(&update(&json!({"isOn": true})), Duration::from_millis(300));
        assert_eq!(waiting.confirmed, None);
        let other: DeviceUpdate =
            serde_json::from_value(json!({"id": "other_1", "attributes": {"lightLevel": 60}}))
                .unwrap();
        waiting.observe(&other, Duration::from_millis(500));
        assert_eq!(waiting.confirmed, None);
        waiting.observe(
            &update(&json!({"lightLevel": 60})),
            Duration::from_millis(2500),
        );
        assert_eq!(waiting.confirmed, Some(Duration::from_millis(2500)));
    }

    #[test]
    fn rounded_whites_and_colors_confirm() {
        let light = strip(
            &json!({"isOn": true, "colorMode": "color", "colorHue": 290.0, "colorSaturation": 0.42}),
        );
        let mut white = Pending::new(&light, &[Change::White(2700)]);
        white.observe(
            &update(&json!({"colorMode": "temperature", "colorTemperature": 2702})),
            Duration::from_millis(300),
        );
        assert!(white.confirmed.is_some());
        let mut color = Pending::new(
            &light,
            &[Change::Color {
                hue: 30.0,
                saturation: 0.8,
            }],
        );
        color.observe(
            &update(&json!({"colorHue": 29.998_168_945_312_5, "colorMode": "color", "colorSaturation": 0.799_212_598_425_196_9})),
            Duration::from_millis(300),
        );
        assert!(color.confirmed.is_some());
    }

    #[test]
    fn changes_read_from_before_to_after() {
        let light = strip(
            &json!({"isOn": true, "lightLevel": 100, "colorMode": "color", "colorHue": 290.0, "colorSaturation": 0.42}),
        );
        assert_eq!(describe(&light, &Change::Power(false)), "on → off");
        assert_eq!(
            describe(&light, &Change::Brightness(40)),
            "brightness 100 → 40 %"
        );
        assert_eq!(describe(&light, &Change::White(2700)), "white 2700 K");
        assert_eq!(
            describe(
                &light,
                &Change::Color {
                    hue: 30.0,
                    saturation: 0.8
                }
            ),
            "color 290° 42 % → 30° 80 %"
        );
        assert_eq!(
            theme::describe_state(&light),
            "on, brightness 100 %, color 290° 42 %"
        );
    }

    #[test]
    fn devices_say_what_they_cannot_do() {
        let outlet = device(json!({
            "id": "outlet_1",
            "deviceType": "outlet",
            "attributes": {"customName": "Kitchen", "isOn": false, "lightLevel": 100},
            "capabilities": {"canReceive": ["isOn", "lightLevel"]},
        }));
        let home = Home {
            devices: vec![outlet],
            rooms: vec![],
            scenes: vec![],
            users: vec![],
            user: None,
            hub: None,
            extra: serde_json::Map::new(),
        };
        let target = [Target::Device(&home.devices[0])];
        let error = plan(&home, &target, Wanted::Brightness(Level::To(40)))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Kitchen cannot dim");
        let error = plan(&home, &target, Wanted::White(White::Warm))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Kitchen cannot show white");
        let plans = plan(&home, &target, Wanted::Toggle).unwrap();
        assert_eq!(plans[0].changes, [Change::Power(true)]);
    }

    #[test]
    fn whites_outside_the_range_are_refused() {
        let light = strip(&json!({"isOn": true}));
        let home = Home {
            devices: vec![light],
            rooms: vec![],
            scenes: vec![],
            users: vec![],
            user: None,
            hub: None,
            extra: serde_json::Map::new(),
        };
        let target = [Target::Device(&home.devices[0])];
        let error = plan(&home, &target, Wanted::White(White::Kelvin(6500)))
            .err()
            .unwrap();
        assert!(
            matches!(
                error,
                AppError::Invalid(ChangeError::White { value: 6500, .. })
            ),
            "{error}"
        );
    }
}
