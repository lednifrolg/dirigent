//! The model read from the synthetic home in `tests/data`.

use std::time::Duration;

use dirigera::{DeviceId, DeviceKind, DeviceUpdate, Home, RoomId, TemperatureRange};
use serde_json::{Value, json};

const HOME: &str = include_str!("data/home.json");

fn home() -> Home {
    serde_json::from_str(HOME).unwrap()
}

fn id(text: &str) -> DeviceId {
    DeviceId(text.to_owned())
}

#[test]
fn home_round_trips_without_loss() {
    let sent: Value = serde_json::from_str(HOME).unwrap();
    let home: Home = serde_json::from_value(sent.clone()).unwrap();
    assert_eq!(serde_json::to_value(&home).unwrap(), sent);
}

#[test]
fn devices_know_what_they_are_and_what_they_take() {
    let home = home();
    let kinds: Vec<(&str, DeviceKind)> = home
        .devices
        .iter()
        .map(|device| (device.name(), device.kind()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("Shelf strip", DeviceKind::Light),
            ("Reading lamp", DeviceKind::Outlet),
            ("Bedside switch", DeviceKind::Controller),
            ("Test hub", DeviceKind::Gateway),
        ]
    );

    let strip = home
        .device(&id("a1b2c3d4-0000-4000-8000-000000000001_1"))
        .unwrap();
    let controls = strip.controls();
    assert!(controls.power && controls.brightness && controls.color);
    assert_eq!(
        controls.white,
        Some(TemperatureRange {
            warmest: 2202,
            coolest: 4000
        })
    );
    assert_eq!(strip.attributes.light_level(), Some(60));
    assert_eq!(
        strip.seen_at(),
        Some("2026-01-01T12:00:00Z".parse().unwrap())
    );

    let outlet = home
        .device(&id("a1b2c3d4-0000-4000-8000-000000000002_1"))
        .unwrap();
    let controls = outlet.controls();
    assert!(controls.power);
    assert!(!controls.brightness && controls.white.is_none() && !controls.color);
    assert_eq!(
        outlet.remote_links(),
        [id("a1b2c3d4-0000-4000-8000-000000000003_1")]
    );

    let remote = home
        .device(&id("a1b2c3d4-0000-4000-8000-000000000003_1"))
        .unwrap();
    assert!(!remote.controls().any());
    assert_eq!(remote.attributes.battery_percentage(), Some(7));
    assert_eq!(remote.attributes.ota_status(), Some("updateAvailable"));

    assert!(!home.hub.as_ref().unwrap().controls().any());
}

#[test]
fn rooms_scenes_and_users() {
    let home = home();
    let bedroom = RoomId("a1b2c3d4-0000-4000-8000-0000000000a2".to_owned());
    let names: Vec<&str> = home
        .devices_in(&bedroom)
        .map(dirigera::Device::name)
        .collect();
    assert_eq!(names, ["Reading lamp", "Bedside switch"]);
    assert_eq!(home.room(&bedroom).unwrap().name, "Bedroom");

    let scene = &home.scenes[0];
    assert_eq!(scene.name(), "Evening");
    assert_eq!(scene.undo_window(), Some(Duration::from_secs(30)));
    assert_eq!(
        scene.actions[0].device_id,
        Some(id("a1b2c3d4-0000-4000-8000-000000000002_1"))
    );
    assert_eq!(
        scene.actions[0].attributes.as_ref().unwrap().is_on(),
        Some(true)
    );

    assert_eq!(home.users.len(), 1);
    assert_eq!(home.user.as_ref().unwrap().name, "dirigent@test");
}

#[test]
fn state_changes_update_the_home() {
    let mut home = home();
    let update: DeviceUpdate = serde_json::from_value(json!({
        "id": "a1b2c3d4-0000-4000-8000-000000000001_1",
        "type": "light",
        "deviceType": "light",
        "attributes": { "isOn": false },
        "isReachable": false,
        "lastSeen": "2026-01-01T12:05:00.000Z",
        "remoteLinks": [],
    }))
    .unwrap();
    assert!(home.apply(&update));
    let strip = home.device(&update.id).unwrap();
    assert_eq!(strip.attributes.is_on(), Some(false));
    assert_eq!(strip.attributes.light_level(), Some(60));
    assert_eq!(strip.is_reachable, Some(false));
    assert_eq!(
        strip.seen_at(),
        Some("2026-01-01T12:05:00Z".parse().unwrap())
    );

    let hub_update: DeviceUpdate = serde_json::from_value(json!({
        "id": "a1b2c3d4-0000-4000-8000-00000000c0de_1",
        "attributes": { "customName": "Renamed hub" },
    }))
    .unwrap();
    assert!(home.apply(&hub_update));
    assert_eq!(home.hub.as_ref().unwrap().name(), "Renamed hub");

    let unknown: DeviceUpdate =
        serde_json::from_value(json!({ "id": "not-in-this-home_1" })).unwrap();
    assert!(!home.apply(&unknown));
}

#[test]
fn scene_updates_update_the_home() {
    let mut home = home();
    let id = "a1b2c3d4-0000-4000-8000-0000000000b1";
    let before = home
        .apply_scene(&json!({ "id": id, "lastTriggered": "2026-01-01T12:00:00.000Z" }))
        .unwrap();
    assert_eq!(
        before.triggered_at(),
        Some("2025-12-31T18:00:00.250Z".parse().unwrap())
    );
    let scene = &home.scenes[0];
    assert_eq!(
        scene.triggered_at(),
        Some("2026-01-01T12:00:00Z".parse().unwrap())
    );
    assert_eq!(scene.name(), "Evening", "what the event leaves out stays");
    assert_eq!(scene.extra["createdAt"], "2024-02-01T17:00:00.000Z");

    let mut renamed = serde_json::to_value(scene).unwrap();
    renamed["info"]["name"] = json!("Late evening");
    home.apply_scene(&renamed).unwrap();
    assert_eq!(home.scenes[0].name(), "Late evening");

    assert!(
        home.apply_scene(&json!({ "id": "not-in-this-home" }))
            .is_none()
    );
}
