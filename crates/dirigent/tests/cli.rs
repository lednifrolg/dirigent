//! End to end: the real `dirigent` binary against a fake hub.

use std::fs;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

mod common;

use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn pairing_waits_for_the_button_and_keeps_the_token_private() {
    let dirigent = Dirigent::new().await;
    dirigent.hub.press_after(1);

    let output = dirigent.run(&["pair", "--name", "dirigent@test"]).await;

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("Press the action button"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("paired with Test hub at 127.0.0.1:"),
        "{}",
        stdout(&output)
    );
    let token_requests: Vec<Value> = dirigent
        .hub
        .requests()
        .into_iter()
        .filter(|r| r.path == "/v1/oauth/token")
        .map(|r| r.body)
        .collect();
    assert_eq!(
        token_requests.len(),
        2,
        "one refused before the press, one after"
    );
    let file = dirigent.file("state/hub.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let stored: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(stored["token"], TOKEN);
    assert_eq!(stored["name"], "Test hub");
    assert_eq!(stored["user"], "a1b2c3d4-0000-4000-8000-0000000000f1");

    let again = dirigent.run(&["pair"]).await;
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    assert!(
        stdout(&again).contains("already paired with Test hub"),
        "{}",
        stdout(&again)
    );
}

/// Pairs without `DIRIGENT_HOME`, so dirigent keeps the token in the locations of the system,
/// which `configure` points into the test's directory.
async fn pair_without_dirigent_home(
    dirigent: &Dirigent,
    configure: impl FnOnce(&mut tokio::process::Command) -> &mut tokio::process::Command,
) {
    let mut command = dirigent.command(&["pair", "--name", "dirigent@test"]);
    configure(command.env_remove("DIRIGENT_HOME"));
    let output = command.output().await.unwrap();
    assert_eq!(code(&output), 0, "{}", stderr(&output));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_token_is_kept_in_the_xdg_state_directory() {
    let dirigent = Dirigent::new().await;
    let home = dirigent.home.path();

    pair_without_dirigent_home(&dirigent, |command| {
        command
            .env("HOME", home.join("home"))
            .env("XDG_STATE_HOME", home.join("xdg-state"))
    })
    .await;

    assert!(home.join("xdg-state/dirigent/hub.json").exists());
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn the_token_is_kept_in_local_app_data() {
    let dirigent = Dirigent::new().await;
    let home = dirigent.home.path();

    pair_without_dirigent_home(&dirigent, |command| {
        command
            .env("APPDATA", home.join("roaming"))
            .env("LOCALAPPDATA", home.join("local"))
    })
    .await;

    assert!(home.join(r"local\dirigent\state\hub.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn completions_and_man_pages_need_no_hub() {
    let dirigent = Dirigent::new().await;
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let output = dirigent.run(&["completions", shell]).await;
        assert_eq!(code(&output), 0, "{shell}: {}", stderr(&output));
        assert!(stdout(&output).contains("dirigent"), "{shell}");
    }
    let pages = dirigent.file("man");
    let output = dirigent.run(&["man", pages.to_str().unwrap()]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    for page in [
        "dirigent.1",
        "dirigent-pair.1",
        "dirigent-dim.1",
        "dirigent-color.1",
        "dirigent-watch.1",
    ] {
        assert!(pages.join(page).exists(), "{page}");
    }
    assert!(!dirigent.file("state").exists(), "nothing else was written");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tui_needs_a_terminal() {
    let dirigent = Dirigent::paired().await;
    let output = dirigent.run(&[]).await;
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("opens the TUI, which needs a terminal"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_need_a_pairing() {
    let dirigent = Dirigent::new().await;
    let output = dirigent.run(&["list"]).await;
    assert_eq!(code(&output), 4);
    assert!(
        stderr(&output).contains("run `dirigent pair`"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_pairing_asks_to_pair_again() {
    let dirigent = Dirigent::paired().await;
    dirigent.hub.remove_user();
    let output = dirigent.run(&["list"]).await;
    assert_eq!(code(&output), 4, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("the hub no longer accepts dirigent"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_groups_devices_by_room() {
    let dirigent = Dirigent::paired().await;
    let output = dirigent.run(&["list"]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    for expected in [
        "Study  1 of 1 on",
        "● Shelf strip   light   on  60 %",
        "Bedroom  0 of 1 on",
        "○ Reading lamp  outlet  off",
        "Scenes   Evening",
        "Remotes  Bedside switch 7 % ⚠",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    assert!(
        !text.contains("Test hub"),
        "the hub is not a device to list\n{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_json_describes_every_device() {
    let dirigent = Dirigent::paired().await;
    let output = dirigent.run(&["list", "--json"]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let list: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(list["hub"], "Test hub");
    let strip = &list["devices"][0];
    assert_eq!(strip["name"], "Shelf strip");
    assert_eq!(strip["brightness"], 60);
    assert_eq!(strip["color"], json!({ "hue": 30.0, "saturation": 0.8 }));
    assert_eq!(
        strip["controls"],
        json!(["power", "brightness", "white", "color"])
    );
    let lamp = &list["devices"][1];
    assert_eq!(lamp["controls"], json!(["power"]));
    assert_eq!(lamp["links"], json!(["Bedside switch"]));
    let remote = &list["devices"][2];
    assert_eq!(remote["kind"], "remote");
    assert_eq!(remote["battery"], 7);
    assert_eq!(remote["links"], json!(["Reading lamp"]));
    assert_eq!(list["scenes"][0]["undo_seconds"], 30);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_is_confirmed_by_the_hub_event() {
    let dirigent = Dirigent::paired().await;
    let output = dirigent.run(&["off", "shelf"]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("✓ Shelf strip  on → off"),
        "{}",
        stdout(&output)
    );
    assert_eq!(
        dirigent.hub.changes(),
        [format!(
            "PATCH /v1/devices/{STRIP} [{{\"attributes\":{{\"isOn\":false}}}}]"
        )]
    );
    assert_eq!(dirigent.hub.device(STRIP)["attributes"]["isOn"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn brightness_white_and_color_take_names_and_steps() {
    let dirigent = Dirigent::paired().await;
    for args in [
        &["dim", "shelf", "-10"][..],
        &["white", "shelf", "warm"],
        &["color", "shelf", "#0000ff"],
    ] {
        let output = dirigent.run(args).await;
        assert_eq!(code(&output), 0, "{args:?}: {}", stderr(&output));
    }
    let strip = dirigent.hub.device(STRIP);
    assert_eq!(strip["attributes"]["lightLevel"], 50);
    assert_eq!(strip["attributes"]["colorTemperature"], 2202);
    assert_eq!(strip["attributes"]["colorHue"], 240.0);
    assert_eq!(strip["attributes"]["colorSaturation"], 1.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rooms_switch_what_they_hold_and_skip_what_shows_already() {
    let dirigent = Dirigent::paired().await;
    let output = dirigent.run(&["off", "bedroom"]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("· Reading lamp  already off"),
        "{}",
        stdout(&output)
    );
    assert_eq!(dirigent.hub.changes(), Vec::<String>::new());

    let output = dirigent.run(&["toggle", "--room", "bedroom"]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("✓ Reading lamp  off → on"),
        "{}",
        stdout(&output)
    );
    assert_eq!(
        dirigent.hub.changes(),
        [format!(
            "PATCH /v1/devices/{LAMP} [{{\"attributes\":{{\"isOn\":true}}}}]"
        )]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_changes_and_unclear_names_send_nothing() {
    let dirigent = Dirigent::paired().await;
    let dim = dirigent.run(&["dim", "reading", "30"]).await;
    assert_eq!(code(&dim), 2);
    assert!(
        stderr(&dim).contains("Reading lamp cannot dim"),
        "{}",
        stderr(&dim)
    );
    let vague = dirigent.run(&["on", "e"]).await;
    assert_eq!(code(&vague), 2);
    assert!(
        stderr(&vague).contains("\"e\" matches"),
        "{}",
        stderr(&vague)
    );
    let unknown = dirigent.run(&["on", "garage"]).await;
    assert_eq!(code(&unknown), 2);
    assert!(
        stderr(&unknown).contains("no device or room named \"garage\""),
        "{}",
        stderr(&unknown)
    );
    let white = dirigent.run(&["white", "shelf", "6500"]).await;
    assert_eq!(code(&white), 2);
    assert!(
        stderr(&white).contains("6500 K is outside 2202-4000 K"),
        "{}",
        stderr(&white)
    );
    assert_eq!(dirigent.hub.changes(), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_never_reports_ends_with_code_5() {
    let dirigent = Dirigent::paired().await;
    dirigent.hub.report(Reporting::Silent);
    let output = dirigent.run(&["off", "shelf"]).await;
    assert_eq!(code(&output), 5, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("the hub shows it, but the device did not report it within 5 s"),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("1 of 1 changes did not show within 5 s"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_wait_returns_once_the_hub_took_it() {
    let dirigent = Dirigent::paired().await;
    dirigent.hub.report(Reporting::Silent);
    let output = dirigent
        .run(&["on", "reading", "--no-wait", "--json"])
        .await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report[0]["name"], "Reading lamp");
    assert_eq!(report[0]["changes"], json!(["on"]));
    assert_eq!(report[0]["result"], "sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn status_shows_a_light() {
    let dirigent = Dirigent::paired().await;
    let output = dirigent.run(&["status", "shelf"]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    for expected in [
        "Shelf strip  light · Study · ORMANAS LED Strip",
        "power       on",
        "brightness  60 %",
        "white       -  (2202-4000 K)",
        // Without colors there is no swatch.
        "color       30° 80 %",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scenes_run_and_undo() {
    let dirigent = Dirigent::paired().await;
    let run = dirigent.run(&["scene", "even"]).await;
    assert_eq!(code(&run), 0, "{}", stderr(&run));
    assert!(
        stdout(&run).contains("ran Evening; within 30 s"),
        "{}",
        stdout(&run)
    );
    let lamp_is_on = || dirigent.hub.device(LAMP)["attributes"]["isOn"].clone();
    assert_eq!(lamp_is_on(), true, "the scene switches the lamp on");
    let undo = dirigent.run(&["scene", "evening", "--undo"]).await;
    assert_eq!(code(&undo), 0, "{}", stderr(&undo));
    assert_eq!(lamp_is_on(), false, "the undo puts it back");
    let scene = "a1b2c3d4-0000-4000-8000-0000000000b1";
    assert_eq!(
        dirigent.hub.changes(),
        [
            format!("POST /v1/scenes/{scene}/trigger null"),
            format!("POST /v1/scenes/{scene}/undo null"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unpairing_removes_dirigent_from_the_hub() {
    let dirigent = Dirigent::paired().await;
    let output = dirigent.run(&["unpair"]).await;
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("unpaired from Test hub"),
        "{}",
        stdout(&output)
    );
    assert_eq!(
        dirigent.hub.changes(),
        ["DELETE /v1/users/a1b2c3d4-0000-4000-8000-0000000000f1 null"]
    );
    assert!(!dirigent.file("state/hub.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_prints_changes_as_they_come() {
    let dirigent = Dirigent::paired().await;
    let mut child = dirigent.command(&["watch"]).spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for _ in 0..100 {
        if dirigent.hub.listeners() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // As the hub sends them: reachable every time, and now and then only when last seen.
    dirigent.hub.broadcast(&json!({
        "type": "deviceStateChanged",
        "data": { "id": STRIP, "isReachable": true, "lastSeen": "2026-01-01T11:59:00.000Z" },
    }));
    dirigent.hub.broadcast(&json!({
        "type": "deviceStateChanged",
        "time": "2026-01-01T12:00:00.000Z",
        "data": { "id": STRIP, "attributes": { "isOn": false }, "isReachable": true },
    }));
    dirigent.hub.broadcast(&json!({
        "type": "remotePressEvent",
        "data": { "id": "a1b2c3d4-0000-4000-8000-000000000003_1", "clickPattern": "singlePress" },
    }));
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("watch prints within 5 s")
            .unwrap()
            .unwrap();
        seen.push(line);
    }
    child.kill().await.unwrap();
    assert!(seen[0].ends_with("Shelf strip     off"), "{seen:?}");
    assert!(seen[1].ends_with("Bedside switch  singlePress"), "{seen:?}");
}
