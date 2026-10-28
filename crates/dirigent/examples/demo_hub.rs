//! A stand-in for the hub with a small home, for demos and screenshots: no request reaches a
//! real hub.
//!
//! ```sh
//! export DIRIGENT_HOME=$(mktemp -d) DIRIGENT_HUB_URL=http://127.0.0.1:18892
//! cargo run -p dirigent --example demo_hub -- 18892 "$DIRIGENT_HOME" &
//! dirigent
//! ```
//!
//! It serves the synthetic test home with a living room added, leaves dirigent paired with it in
//! the given home directory, reports a change a moment after it arrives, with a light fading
//! through another brightness first, and every few seconds a remote switches the floor lamp, as
//! someone at home would.

#[path = "../tests/common/hub.rs"]
mod hub;

use std::path::Path;
use std::time::Duration;

use hub::{FakeHub, HOME, Reporting};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const USAGE: &str = "usage: demo_hub <port> <new home directory>";
const LIVING_ROOM: &str = "a1b2c3d4-0000-4000-8000-0000000000a3";
const FLOOR_LAMP: &str = "a1b2c3d4-0000-4000-8000-000000000012_1";
const SOFA_SWITCH: &str = "a1b2c3d4-0000-4000-8000-000000000013_1";
/// How often the remote is pressed.
const PRESS_EVERY: Duration = Duration::from_secs(12);

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let Some(port) = args.next().and_then(|p| p.parse::<u16>().ok()) else {
        fail(USAGE);
    };
    // An unset variable, as in `"$DIRIGENT_HOME"`, would make it the current directory.
    let Some(home) = args.next().filter(|home| !home.is_empty()) else {
        fail(USAGE);
    };
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)).await else {
        fail(&format!("port {port} is taken"));
    };
    let hub = FakeHub::start_with(listener, demo_home());
    hub.report(Reporting::Fading);
    hub.write_pairing(Path::new(&home));
    println!("demo hub on http://{}", hub.address);
    loop {
        tokio::time::sleep(PRESS_EVERY).await;
        if hub.listeners() > 0 {
            hub.press(SOFA_SWITCH, FLOOR_LAMP);
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

/// The synthetic home with a living room: a white ceiling light, a floor lamp on an outlet with
/// the remote that switches it, and a scene for the evening.
fn demo_home() -> Value {
    let mut home: Value = serde_json::from_str(HOME).expect("synthetic data");
    let room = json!({
        "id": LIVING_ROOM,
        "name": "Living room",
        "color": "ikea_yellow_no_24",
        "icon": "rooms_sofa",
    });
    let template = |index: usize| home["devices"][index].clone();
    let mut ceiling = template(0);
    ceiling["id"] = json!("a1b2c3d4-0000-4000-8000-000000000011_1");
    ceiling["room"] = room.clone();
    ceiling["attributes"]["customName"] = json!("Ceiling light");
    ceiling["attributes"]["model"] = json!("TRADFRI bulb E27 WS globe 1055lm");
    ceiling["attributes"]["colorMode"] = json!("temperature");
    ceiling["attributes"]["colorTemperature"] = json!(2700);
    ceiling["attributes"]["lightLevel"] = json!(80);
    ceiling["capabilities"]["canReceive"] =
        json!(["customName", "isOn", "lightLevel", "colorTemperature"]);
    let mut lamp = template(1);
    lamp["id"] = json!(FLOOR_LAMP);
    lamp["room"] = room.clone();
    lamp["attributes"]["customName"] = json!("Floor lamp");
    lamp["attributes"]["isOn"] = json!(true);
    lamp["remoteLinks"] = json!([SOFA_SWITCH]);
    let mut remote = template(2);
    remote["id"] = json!(SOFA_SWITCH);
    remote["room"] = room.clone();
    remote["attributes"]["customName"] = json!("Sofa switch");
    remote["attributes"]["batteryPercentage"] = json!(64);
    remote["attributes"]["otaStatus"] = json!("upToDate");
    let devices = home["devices"].as_array_mut().expect("synthetic devices");
    devices.splice(0..0, [ceiling, lamp, remote]);
    home["rooms"]
        .as_array_mut()
        .expect("synthetic rooms")
        .insert(0, room);
    let mut scene = home["scenes"][0].clone();
    scene["id"] = json!("a1b2c3d4-0000-4000-8000-0000000000b3");
    scene["info"]["name"] = json!("Movie night");
    scene["actions"] = json!([
        { "id": "a1b2c3d4-0000-4000-8000-000000000011_1", "type": "device", "deviceId": "a1b2c3d4-0000-4000-8000-000000000011_1", "attributes": { "isOn": false } },
        { "id": FLOOR_LAMP, "type": "device", "deviceId": FLOOR_LAMP, "attributes": { "isOn": true } },
    ]);
    home["scenes"]
        .as_array_mut()
        .expect("synthetic scenes")
        .push(scene);
    home
}
