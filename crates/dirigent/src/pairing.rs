//! Pairing with a hub, for `dirigent pair` and the TUI's pairing dialog alike.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use dirigera::{API_PORT, Client, Discovered, PRESS_WINDOW, Pairing, Token};
use jiff::Timestamp;

use crate::connect::{self, Hub};
use crate::error::AppError;
use crate::paths::Paths;
use crate::store::Paired;

pub const DISCOVERY_WAIT: Duration = Duration::from_secs(3);

/// How the hub lists dirigent among its users: `dirigent@` and this computer's name.
pub fn client_name() -> String {
    let host = gethostname::gethostname();
    let host = host.to_string_lossy();
    let host = host.split('.').next().unwrap_or_default();
    if host.is_empty() {
        "dirigent".to_owned()
    } else {
        format!("dirigent@{host}")
    }
}

fn address(text: &str) -> Result<SocketAddr, AppError> {
    if let Ok(address) = text.parse() {
        return Ok(address);
    }
    text.parse::<IpAddr>()
        .map(|ip| SocketAddr::new(ip, API_PORT))
        .map_err(|_| AppError::Usage(format!("{text} is not an IP address")))
}

/// Where to pair: the test double, the `given` address, or the one hub on the network.
pub async fn find_hub(given: Option<&str>) -> Result<(SocketAddr, Option<Discovered>), AppError> {
    if let Some(address) = connect::test_hub()? {
        return Ok((address, None));
    }
    if let Some(text) = given {
        return Ok((address(text)?, None));
    }
    let mut hubs = dirigera::discover(DISCOVERY_WAIT).await?;
    match hubs.len() {
        0 => Err(AppError::Unreachable(
            "no hub answered on the network; give its address: dirigent pair 192.168.1.20".into(),
        )),
        1 => {
            let hub = hubs.remove(0);
            let address = *hub.addresses.first().ok_or_else(|| {
                AppError::Unreachable(format!("the hub {} announced no address", hub.hostname))
            })?;
            Ok((address, Some(hub)))
        }
        _ => {
            let list: Vec<String> = hubs
                .iter()
                .map(|hub| {
                    let at = hub
                        .addresses
                        .first()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    format!("{} at {at}", hub.hostname)
                })
                .collect();
            Err(AppError::Usage(format!(
                "several hubs answered: {}; pick one: dirigent pair <address>",
                list.join(", ")
            )))
        }
    }
}

/// Asks the hub at `address` for a pairing code; this is when its certificate is pinned.
pub async fn start(address: SocketAddr) -> Result<Pairing, AppError> {
    Ok(if connect::test_hub()?.is_some() {
        Pairing::start_plain(address).await?
    } else {
        Pairing::start(address).await?
    })
}

/// Asks for the token about once a second until the button is pressed. `waiting` hears how
/// long the user has left.
pub async fn wait_for_press(
    pairing: &Pairing,
    client_name: &str,
    mut waiting: impl FnMut(Duration),
) -> Result<Token, AppError> {
    let started = Instant::now();
    loop {
        if let Some(token) = pairing.finish(client_name).await? {
            return Ok(token);
        }
        let left = PRESS_WINDOW.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(AppError::Unreachable(format!(
                "the button was not pressed within {} s",
                PRESS_WINDOW.as_secs()
            )));
        }
        waiting(left);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Reads the home with the new `token` and stores the pairing; the hub is connected then.
pub async fn remember(
    paths: &Paths,
    address: SocketAddr,
    found: Option<Discovered>,
    pairing: &Pairing,
    token: Token,
) -> Result<Hub, AppError> {
    let fingerprint = pairing.fingerprint();
    let client = match fingerprint {
        Some(pin) => Client::new(address, pin, token.clone())?,
        None => Client::plain(address, token.clone())?,
    };
    let home = client.home().await?;
    let hub_device = home.hub.as_ref();
    let uuid = found.as_ref().map_or_else(
        || {
            hub_device
                .map(|hub| hub.id.as_str().trim_end_matches("_1").to_owned())
                .unwrap_or_default()
        },
        |hub| hub.uuid.clone(),
    );
    let name = hub_device
        .map(|hub| hub.name().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| found.as_ref().map(|hub| hub.hostname.clone()))
        .unwrap_or_else(|| "DIRIGERA".to_owned());
    let mut paired = Paired {
        uuid,
        name,
        addresses: vec![address],
        fingerprint: fingerprint.map(|pin| pin.to_string()),
        token: token.expose().to_owned(),
        user: home.user.as_ref().map(|user| user.uid.to_string()),
        since: Timestamp::now(),
    };
    if let Some(found) = found {
        paired.prefer(address, found.addresses);
    }
    paired.save(paths)?;
    Ok(Hub {
        client,
        home,
        paired,
    })
}
