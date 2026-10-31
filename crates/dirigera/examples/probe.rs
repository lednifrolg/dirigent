//! Live checks against a real hub while the `dirigent` CLI does not exist yet.
//!
//! Every command except `discover`, `pin` and `pair` reads the hub from the environment:
//! `DIRIGERA_IP` (address, optionally with port), `DIRIGERA_TOKEN` and `DIRIGERA_PIN` (the
//! certificate fingerprint `pin` prints). Commands that change something change real devices.
//!
//! ```sh
//! cargo run -p dirigera --example probe -- discover
//! cargo run -p dirigera --example probe -- pair 192.168.1.20 dirigent-probe hub.env
//! cargo run -p dirigera --example probe -- apply <device-id> level 40
//! ```

use std::fs::OpenOptions;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use dirigera::{
    API_PORT, Change, Client, DeviceId, EventKind, EventStream, Fingerprint, PRESS_WINDOW, Pairing,
    SceneId, Token, UserId,
};

const USAGE: &str = "usage: probe discover
       probe pin <ip>
       probe pair <ip> <client-name> <out-file>
       probe home
       probe apply <device-id> on|off|level <percent>|white <kelvin>|color <hue> <saturation>
       probe identify <device-id> <seconds>
       probe scene <scene-id> trigger|undo
       probe users
       probe remove-user <user-id>
       probe events <seconds>";

type Result<T = (), E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: &[&str]) -> Result {
    match args {
        ["discover"] => discover().await,
        ["pin", ip] => {
            let pairing = Pairing::start(address(ip)?).await?;
            println!("{}", pairing.fingerprint().ok_or("no certificate")?);
            Ok(())
        }
        ["pair", ip, name, out] => pair(address(ip)?, name, out).await,
        ["home"] => home(&client()?).await,
        ["apply", id, change @ ..] => {
            let client = client()?;
            let device = client.device(&DeviceId((*id).to_owned())).await?;
            let change = parse_change(change)?;
            let sent = Instant::now();
            client.apply(&device, &change).await?;
            println!(
                "{}: {change} accepted in {:?}",
                device.name(),
                sent.elapsed()
            );
            Ok(())
        }
        ["identify", id, seconds] => {
            let period = Duration::from_secs(seconds.parse()?);
            client()?
                .identify(&DeviceId((*id).to_owned()), period)
                .await?;
            Ok(())
        }
        ["scene", id, "trigger"] => Ok(client()?.trigger_scene(&SceneId((*id).to_owned())).await?),
        ["scene", id, "undo"] => Ok(client()?.undo_scene(&SceneId((*id).to_owned())).await?),
        ["users"] => {
            for user in client()?.users().await? {
                let audience = user.audience.as_deref().unwrap_or("-");
                let created = user.created_timestamp.as_deref().unwrap_or("-");
                println!("{}  {:<28} {audience:<18} {created}", user.uid, user.name);
            }
            Ok(())
        }
        ["remove-user", id] => Ok(client()?.remove_user(&UserId((*id).to_owned())).await?),
        ["events", seconds] => events(&client()?, Duration::from_secs(seconds.parse()?)).await,
        _ => Err(USAGE.into()),
    }
}

async fn discover() -> Result {
    for hub in dirigera::discover(Duration::from_secs(3)).await? {
        let addresses: Vec<String> = hub.addresses.iter().map(ToString::to_string).collect();
        println!(
            "{}  {}  fw {}  api {}  {}",
            hub.uuid,
            hub.hostname,
            hub.firmware.as_deref().unwrap_or("-"),
            hub.api_version.as_deref().unwrap_or("-"),
            addresses.join(", ")
        );
    }
    Ok(())
}

async fn pair(address: SocketAddr, name: &str, out: &str) -> Result {
    let pairing = Pairing::start(address).await?;
    let pin = pairing.fingerprint().ok_or("no certificate")?;
    println!("certificate {pin}");
    println!("press the action button on the bottom of the hub");
    let started = Instant::now();
    let mut asked = 0;
    let token = loop {
        asked += 1;
        if let Some(token) = pairing.finish(name).await? {
            break token;
        }
        if started.elapsed() > PRESS_WINDOW {
            return Err(format!("no button press within {PRESS_WINDOW:?}").into());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    println!("paired after {asked} requests in {:?}", started.elapsed());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(out)?;
    writeln!(
        file,
        "DIRIGERA_IP={address}\nDIRIGERA_TOKEN={}\nDIRIGERA_PIN={pin}",
        token.expose()
    )?;
    println!("saved to {out}");
    Ok(())
}

async fn home(client: &Client) -> Result {
    let home = client.home().await?;
    for device in &home.devices {
        let a = &device.attributes;
        let controls = device.controls();
        let room = device.room.as_ref().map_or("-", |room| room.name.as_str());
        println!(
            "{:<16} {:<16} {:<12} on={:<5} level={:<4} white={:<5} hue={:<6} sat={:<5} battery={:<4} controls={}{}{}{}  {}",
            device.name(),
            device.device_type,
            room,
            show(a.is_on()),
            show(a.light_level()),
            show(a.color_temperature()),
            show(a.color_hue().map(|h| format!("{h:.1}"))),
            show(a.color_saturation().map(|s| format!("{s:.2}"))),
            show(a.battery_percentage()),
            if controls.power { "P" } else { "-" },
            if controls.brightness { "B" } else { "-" },
            if controls.white.is_some() { "W" } else { "-" },
            if controls.color { "C" } else { "-" },
            device.id,
        );
    }
    for scene in &home.scenes {
        println!(
            "scene {:<16} undo {:?}  {}",
            scene.name(),
            scene.undo_window(),
            scene.id
        );
    }
    Ok(())
}

async fn events(client: &Client, wait: Duration) -> Result {
    let mut stream = EventStream::connect(client).await?;
    let started = Instant::now();
    println!("listening for {wait:?}");
    while let Some(left) = wait.checked_sub(started.elapsed()) {
        let Ok(next) = tokio::time::timeout(left, stream.next()).await else {
            break;
        };
        let Some(event) = next? else {
            println!(
                "{:>7.3}s  the hub closed the stream",
                started.elapsed().as_secs_f64()
            );
            break;
        };
        let at = started.elapsed().as_secs_f64();
        match &event.kind {
            EventKind::DeviceStateChanged(update) => {
                let attributes: Vec<String> = update
                    .attributes
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                println!(
                    "{at:>7.3}s  {}  {}  {}",
                    event.event_type,
                    update.id,
                    attributes.join(" ")
                );
            }
            _ => println!("{at:>7.3}s  {}  {}", event.event_type, event.raw["data"]),
        }
    }
    stream.close().await;
    Ok(())
}

fn client() -> Result<Client> {
    let ip = std::env::var("DIRIGERA_IP").map_err(|_| "DIRIGERA_IP is not set")?;
    let token = std::env::var("DIRIGERA_TOKEN").map_err(|_| "DIRIGERA_TOKEN is not set")?;
    let pin: Fingerprint = std::env::var("DIRIGERA_PIN")
        .map_err(|_| "DIRIGERA_PIN is not set; `probe pin <ip>` prints it")?
        .parse()?;
    Ok(Client::new(address(&ip)?, pin, Token::new(token))?)
}

fn address(text: &str) -> Result<SocketAddr> {
    if let Ok(address) = text.parse() {
        return Ok(address);
    }
    let ip: IpAddr = text.parse()?;
    Ok(SocketAddr::new(ip, API_PORT))
}

fn parse_change(args: &[&str]) -> Result<Change> {
    Ok(match args {
        ["on"] => Change::Power(true),
        ["off"] => Change::Power(false),
        ["level", percent] => Change::Brightness(percent.parse()?),
        ["white", kelvin] => Change::White(kelvin.parse()?),
        ["color", hue, saturation] => Change::Color {
            hue: hue.parse()?,
            saturation: saturation.parse()?,
        },
        _ => return Err(USAGE.into()),
    })
}

fn show<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| v.to_string())
}
