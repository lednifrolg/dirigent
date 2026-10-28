//! Commands that pair, look around and run scenes; [`crate::control`] has the ones that change
//! devices.

use std::io::IsTerminal;
use std::time::Duration;

use anstream::eprint;
use dirigera::{Backoff, Device, DeviceKind, EventKind, EventStream, Home, Scene, UserId};
use jiff::Timestamp;
use serde::Serialize;

use crate::connect::Hub;
use crate::error::AppError;
use crate::names::{Kind, Target};
use crate::output::{self, BOLD, DIM, LOW_BATTERY, YELLOW, paint};
use crate::pairing::{self, DISCOVERY_WAIT};
use crate::paths::Paths;
use crate::store::Paired;
use crate::theme;
use crate::wanted::Color;

pub async fn pair(
    paths: &Paths,
    given: Option<&str>,
    name: Option<String>,
) -> Result<(), AppError> {
    if Paired::load(paths)?.is_some() {
        match Hub::connect(paths).await {
            Ok(hub) => {
                output::success(format_args!(
                    "already paired with {} at {}; `dirigent unpair` removes the pairing",
                    hub.paired.name,
                    hub.client.address()
                ));
                return Ok(());
            }
            Err(AppError::TokenRevoked | AppError::CertificateChanged) => {
                output::note("the stored pairing no longer works; pairing again");
            }
            Err(error) => return Err(error),
        }
    }
    let (address, found) = pairing::find_hub(given).await?;
    let started = pairing::start(address).await?;
    let client_name = name.unwrap_or_else(pairing::client_name);
    output::line(format_args!(
        "Press the action button on the bottom of the hub at {address}."
    ));
    // A countdown on a terminal, rewritten in place.
    let countdown = std::io::stderr().is_terminal();
    let token = pairing::wait_for_press(&started, &client_name, |left| {
        if countdown {
            eprint!("\r{}", paint(DIM, format!("waiting {} s", left.as_secs())));
        }
    })
    .await;
    if countdown {
        eprint!("\r\x1b[2K");
    }
    let token = token.map_err(|error| match error {
        AppError::Unreachable(why) => {
            AppError::Unreachable(format!("{why}; run `dirigent pair` again"))
        }
        other => other,
    })?;
    let hub = pairing::remember(paths, address, found, &started, token).await?;
    output::success(format_args!(
        "paired with {} at {address}; the hub lists dirigent as {client_name}",
        hub.paired.name
    ));
    output::note(format_args!(
        "the token is in {}, readable only by you",
        paths.hub().display()
    ));
    Ok(())
}

pub async fn unpair(paths: &Paths) -> Result<(), AppError> {
    let Some(paired) = Paired::load(paths)? else {
        output::note("not paired with a hub; nothing to remove");
        return Ok(());
    };
    match Hub::connect(paths).await {
        Ok(hub) => {
            let user = hub
                .home
                .user
                .as_ref()
                .map(|user| user.uid.clone())
                .or_else(|| paired.user.clone().map(UserId));
            match user {
                Some(user) => hub.client.remove_user(&user).await?,
                None => output::warn(
                    "the hub did not say which user dirigent is; remove it in the IKEA app",
                ),
            }
        }
        Err(AppError::TokenRevoked) => output::note("the hub had removed dirigent already"),
        Err(error) => return Err(error),
    }
    Paired::forget(paths)?;
    output::success(format_args!("unpaired from {}", paired.name));
    Ok(())
}

#[derive(Serialize)]
struct FoundHub {
    uuid: String,
    hostname: String,
    addresses: Vec<String>,
    firmware: Option<String>,
    api_version: Option<String>,
}

pub async fn discover(json: bool) -> Result<(), AppError> {
    let hubs = dirigera::discover(DISCOVERY_WAIT).await?;
    if json {
        let hubs: Vec<FoundHub> = hubs
            .into_iter()
            .map(|hub| FoundHub {
                uuid: hub.uuid,
                hostname: hub.hostname,
                addresses: hub.addresses.iter().map(ToString::to_string).collect(),
                firmware: hub.firmware,
                api_version: hub.api_version,
            })
            .collect();
        return output::json(&hubs);
    }
    if hubs.is_empty() {
        output::note(format_args!(
            "no hub answered within {} s",
            DISCOVERY_WAIT.as_secs()
        ));
        return Ok(());
    }
    for hub in hubs {
        let addresses: Vec<String> = hub.addresses.iter().map(ToString::to_string).collect();
        output::line(format_args!(
            "{}  {}  {}",
            paint(BOLD, &hub.hostname),
            addresses.join(", "),
            paint(
                DIM,
                format!("firmware {}", hub.firmware.as_deref().unwrap_or("?"))
            )
        ));
    }
    Ok(())
}

/// A device as `list --json` and `status --json` show it.
#[derive(Serialize)]
struct DeviceView {
    id: String,
    name: String,
    kind: &'static str,
    room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reachable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brightness: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    white: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<ColorView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    battery: Option<u8>,
    /// What a remote switches, or the remotes that switch a device.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    links: Vec<String>,
    controls: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    firmware: Option<String>,
    update_available: bool,
}

#[derive(Serialize)]
struct ColorView {
    hue: f64,
    saturation: f64,
}

impl DeviceView {
    fn new(home: &Home, device: &Device) -> Self {
        let a = &device.attributes;
        let controls = device.controls();
        let mut can = Vec::new();
        for (has, name) in [
            (controls.power, "power"),
            (controls.brightness, "brightness"),
            (controls.white.is_some(), "white"),
            (controls.color, "color"),
        ] {
            if has {
                can.push(name);
            }
        }
        let color_mode = a.color_mode();
        Self {
            id: device.id.to_string(),
            name: device.name().to_owned(),
            kind: output::kind(device),
            room: device.room.as_ref().map(|room| room.name.clone()),
            reachable: device.is_reachable,
            on: a.is_on().filter(|_| controls.power),
            brightness: a.light_level().filter(|_| controls.brightness),
            white: a.color_temperature().filter(|_| {
                controls.white.is_some() && color_mode == Some(dirigera::ColorMode::Temperature)
            }),
            color: match (color_mode, a.color_hue(), a.color_saturation()) {
                (Some(dirigera::ColorMode::Color), Some(hue), Some(saturation))
                    if controls.color =>
                {
                    Some(ColorView { hue, saturation })
                }
                _ => None,
            },
            battery: a.battery_percentage(),
            links: output::names(home, &links(home, device)),
            controls: can,
            firmware: a.firmware_version().map(str::to_owned),
            update_available: a.ota_status() == Some("updateAvailable"),
        }
    }
}

/// What a remote switches, or the remotes that switch a device.
fn links(home: &Home, device: &Device) -> Vec<dirigera::DeviceId> {
    if device.kind() == DeviceKind::Controller {
        home.devices
            .iter()
            .filter(|other| other.remote_links().contains(&device.id))
            .map(|other| other.id.clone())
            .collect()
    } else {
        device.remote_links().to_vec()
    }
}

#[derive(Serialize)]
struct SceneView {
    id: String,
    name: String,
    actions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    undo_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run: Option<Timestamp>,
}

impl SceneView {
    fn new(scene: &Scene) -> Self {
        Self {
            id: scene.id.to_string(),
            name: scene.name().to_owned(),
            actions: scene.actions.len(),
            undo_seconds: scene.undo_window().map(|window| window.as_secs()),
            last_run: scene.triggered_at(),
        }
    }
}

#[derive(Serialize)]
struct HomeView {
    hub: String,
    devices: Vec<DeviceView>,
    rooms: Vec<String>,
    scenes: Vec<SceneView>,
}

/// Devices the user can do something with or should know about: not the hub itself.
fn shown(home: &Home) -> impl Iterator<Item = &Device> {
    home.devices
        .iter()
        .filter(|device| device.kind() != DeviceKind::Gateway)
}

pub fn list(hub: &Hub, json: bool) -> Result<(), AppError> {
    let home = &hub.home;
    if json {
        return output::json(&HomeView {
            hub: hub.paired.name.clone(),
            devices: shown(home)
                .map(|device| DeviceView::new(home, device))
                .collect(),
            rooms: home.rooms.iter().map(|room| room.name.clone()).collect(),
            scenes: home.scenes.iter().map(SceneView::new).collect(),
        });
    }
    let switchable = |device: &&Device| device.kind() != DeviceKind::Controller;
    // Remotes are listed on one line of their own, so only the rest sets the column.
    let width = shown(home)
        .filter(switchable)
        .map(|d| d.name().chars().count())
        .max()
        .unwrap_or(0);
    let mut groups: Vec<(Option<&dirigera::Room>, Vec<&Device>)> = home
        .rooms
        .iter()
        .map(|room| {
            (
                Some(room),
                home.devices_in(&room.id).filter(switchable).collect(),
            )
        })
        .collect();
    let roomless: Vec<&Device> = shown(home)
        .filter(switchable)
        .filter(|device| device.room.is_none())
        .collect();
    if !roomless.is_empty() {
        groups.push((None, roomless));
    }
    for (room, devices) in groups.iter().filter(|(_, devices)| !devices.is_empty()) {
        let on = devices
            .iter()
            .filter(|d| d.attributes.is_on() == Some(true))
            .count();
        output::line(format_args!(
            "{}  {}",
            output::heading(*room),
            paint(DIM, format!("{on} of {} on", devices.len()))
        ));
        for device in devices {
            output::line(format_args!(
                "  {} {}  {}  {}",
                output::dot(device),
                output::pad(device.name(), width),
                paint(DIM, output::pad(output::kind(device), 6)),
                output::state(device)
            ));
        }
    }
    if !home.scenes.is_empty() {
        let names: Vec<&str> = home.scenes.iter().map(Scene::name).collect();
        output::line(format_args!(
            "{}  {}",
            paint(DIM, "Scenes "),
            names.join(", ")
        ));
    }
    let remotes: Vec<String> = shown(home)
        .filter(|device| device.kind() == DeviceKind::Controller)
        .map(|remote| match remote.attributes.battery_percentage() {
            Some(level) if level < LOW_BATTERY => {
                format!(
                    "{} {}",
                    remote.name(),
                    paint(YELLOW, format!("{level} % ⚠"))
                )
            }
            Some(level) => format!("{} {level} %", remote.name()),
            None => remote.name().to_owned(),
        })
        .collect();
    if !remotes.is_empty() {
        output::line(format_args!(
            "{}  {}",
            paint(DIM, "Remotes"),
            remotes.join(" · ")
        ));
    }
    Ok(())
}

pub fn status(hub: &Hub, target: Target<'_>, json: bool) -> Result<(), AppError> {
    match target {
        Target::Device(device) => device_status(&hub.home, device, json),
        Target::Room(room) => room_status(&hub.home, room, json),
        Target::Scene(scene) => scene_status(&hub.home, scene, json),
    }
}

fn device_status(home: &Home, device: &Device, json: bool) -> Result<(), AppError> {
    let view = DeviceView::new(home, device);
    if json {
        return output::json(&view);
    }
    let mut header = vec![view.kind.to_owned()];
    header.extend(view.room.clone());
    header.extend(device.attributes.model().map(str::to_owned));
    output::line(format_args!(
        "{}  {}",
        paint(BOLD, &view.name),
        paint(DIM, header.join(" · "))
    ));
    let row = |label: &str, value: String| {
        output::line(format_args!(
            "  {}  {value}",
            paint(DIM, output::pad(label, 10))
        ));
    };
    if let Some(on) = view.on {
        row("power", if on { "on".into() } else { "off".into() });
    }
    if let Some(level) = view.brightness {
        row("brightness", format!("{level} %"));
    }
    if let Some(range) = device.controls().white {
        let shown = view
            .white
            .map_or_else(|| "-".to_owned(), |k| format!("{k} K"));
        row(
            "white",
            format!("{shown}  {}", paint(DIM, format!("({range})"))),
        );
    }
    if let Some(color) = &view.color {
        let swatch = output::light_color(device)
            .map(|rgb| paint(rgb.on_default(), "■ "))
            .unwrap_or_default();
        row(
            "color",
            format!(
                "{swatch}{}",
                Color {
                    hue: color.hue,
                    saturation: color.saturation
                }
            ),
        );
    }
    if let Some(level) = view.battery {
        let text = if level < LOW_BATTERY {
            paint(YELLOW, format!("{level} %, replace it"))
        } else {
            format!("{level} %")
        };
        row("battery", text);
    }
    if !view.links.is_empty() {
        let label = if device.kind() == DeviceKind::Controller {
            "switches"
        } else {
            "remote"
        };
        row(label, view.links.join(", "));
    }
    let reachable = match device.is_reachable {
        Some(false) => paint(YELLOW, "no"),
        _ => "yes".to_owned(),
    };
    let seen = device
        .seen_at()
        .map(|at| format!(" · seen {}", theme::ago(at, Timestamp::now())))
        .unwrap_or_default();
    row("reachable", format!("{reachable}{}", paint(DIM, seen)));
    if let Some(firmware) = &view.firmware {
        let update = if view.update_available {
            paint(DIM, " · update available")
        } else {
            String::new()
        };
        row("firmware", format!("{firmware}{update}"));
    }
    row("id", paint(DIM, &view.id));
    Ok(())
}

fn room_status(home: &Home, room: &dirigera::Room, json: bool) -> Result<(), AppError> {
    let devices: Vec<&Device> = home.devices_in(&room.id).collect();
    if json {
        let views: Vec<DeviceView> = devices.iter().map(|d| DeviceView::new(home, d)).collect();
        return output::json(&views);
    }
    output::line(output::heading(Some(room)));
    let width = devices
        .iter()
        .map(|d| d.name().chars().count())
        .max()
        .unwrap_or(0);
    for device in devices {
        output::line(format_args!(
            "  {} {}  {}  {}",
            output::dot(device),
            output::pad(device.name(), width),
            paint(DIM, output::pad(output::kind(device), 6)),
            output::state(device)
        ));
    }
    Ok(())
}

fn scene_status(home: &Home, scene: &Scene, json: bool) -> Result<(), AppError> {
    let view = SceneView::new(scene);
    if json {
        return output::json(&view);
    }
    output::line(format_args!(
        "{}  {}",
        paint(BOLD, scene.name()),
        paint(DIM, "scene")
    ));
    for action in &scene.actions {
        let device = action
            .device_id
            .as_ref()
            .and_then(|id| home.device(id))
            .map_or("a device", Device::name);
        let what = match action
            .attributes
            .as_ref()
            .and_then(dirigera::Attributes::is_on)
        {
            Some(true) => "on",
            Some(false) => "off",
            None => "changed",
        };
        output::line(format_args!("  {device} → {what}"));
    }
    if let Some(at) = view.last_run {
        output::line(format_args!(
            "  {}",
            paint(
                DIM,
                format!("last run {}", theme::ago(at, Timestamp::now()))
            )
        ));
    }
    Ok(())
}

pub async fn scene(hub: &Hub, scene: &Scene, undo: bool) -> Result<(), AppError> {
    if undo {
        hub.client.undo_scene(&scene.id).await?;
        output::success(format_args!("took back the latest run of {}", scene.name()));
        return Ok(());
    }
    hub.client.trigger_scene(&scene.id).await?;
    let hint = scene
        .undo_window()
        .map(|window| {
            format!(
                "; within {} s `dirigent scene \"{}\" --undo` takes it back",
                window.as_secs(),
                scene.name()
            )
        })
        .unwrap_or_default();
    output::success(format_args!("ran {}{hint}", scene.name()));
    Ok(())
}

pub async fn identify(hub: &Hub, device: &Device, seconds: u64) -> Result<(), AppError> {
    hub.client
        .identify(&device.id, Duration::from_secs(seconds))
        .await?;
    output::success(format_args!("{} blinks for {seconds} s", device.name()));
    Ok(())
}

/// Prints every change as the hub reports it, reconnecting when the stream drops.
pub async fn watch(paths: &Paths, json: bool) -> Result<(), AppError> {
    let mut backoff = Backoff::default();
    loop {
        let hub = match Hub::connect(paths).await {
            Ok(hub) => hub,
            Err(error @ AppError::Unreachable(_)) => {
                let delay = backoff.next_delay();
                output::warn(format_args!(
                    "{}; trying again in {} s",
                    output::message(&error),
                    delay.as_secs()
                ));
                tokio::time::sleep(delay).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut home = hub.home;
        let mut stream = match EventStream::connect(&hub.client).await {
            Ok(stream) => stream,
            Err(
                error
                @ (dirigera::Error::Unauthorized | dirigera::Error::CertificateChanged { .. }),
            ) => {
                return Err(error.into());
            }
            Err(error) => {
                let delay = backoff.next_delay();
                output::warn(format_args!(
                    "{error}; trying again in {} s",
                    delay.as_secs()
                ));
                tokio::time::sleep(delay).await;
                continue;
            }
        };
        if !json {
            output::note(format_args!("watching {}; Ctrl-C stops", hub.paired.name));
        }
        let ended = loop {
            match stream.next().await {
                Ok(Some(event)) => {
                    backoff.reset();
                    show_event(&mut home, &event, json);
                }
                Ok(None) => break "the hub closed the event stream".to_owned(),
                Err(error) => break error.to_string(),
            }
        };
        let delay = backoff.next_delay();
        output::warn(format_args!(
            "{ended}; reconnecting in {} s",
            delay.as_secs()
        ));
        tokio::time::sleep(delay).await;
    }
}

fn show_event(home: &mut Home, event: &dirigera::Event, json: bool) {
    if event.kind == EventKind::Pong {
        return;
    }
    if json {
        output::line(serde_json::to_string(&event.raw).expect("an event is JSON"));
        return;
    }
    let time = event
        .time
        .unwrap_or_else(Timestamp::now)
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%H:%M:%S")
        .to_string();
    let (who, what) = match &event.kind {
        EventKind::DeviceStateChanged(update) => {
            let was_reachable = home.device(&update.id).and_then(|d| d.is_reachable);
            home.apply(update);
            let name = home
                .device(&update.id)
                .map_or_else(|| update.id.to_string(), |d| d.name().to_owned());
            let what = theme::describe_update(update, was_reachable);
            if what.is_empty() {
                // Only when it was last seen, or what it already was.
                return;
            }
            (name, what)
        }
        EventKind::RemotePress { device, pattern } => {
            let name = home
                .device(device)
                .map_or_else(|| device.to_string(), |d| d.name().to_owned());
            (name, pattern.clone())
        }
        EventKind::Scene(id) => {
            let name = home
                .scene(id)
                .map_or_else(|| id.to_string(), |s| s.name().to_owned());
            (
                format!("scene {name}"),
                event.event_type.trim_start_matches("scene").to_lowercase(),
            )
        }
        _ => (event.event_type.clone(), String::new()),
    };
    output::line(format_args!(
        "{}  {}  {what}",
        paint(DIM, time),
        output::pad(&who, 14)
    ));
}

/// Resolves the names a command was given into devices and rooms.
pub fn targets<'a>(
    home: &'a Home,
    names: &[String],
    kinds: &[Kind],
) -> Result<Vec<Target<'a>>, AppError> {
    names
        .iter()
        .map(|name| crate::names::resolve(home, name, kinds))
        .collect()
}
