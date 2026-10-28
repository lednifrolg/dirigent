//! Reaching the paired hub: at the address that worked last, else wherever mDNS finds it.

use std::net::SocketAddr;
use std::time::Duration;

use dirigera::{Client, Fingerprint, Home};
use tokio::time::timeout;

use crate::error::AppError;
use crate::paths::Paths;
use crate::store::Paired;

/// The hub answers within milliseconds on the local network; an address that takes longer is
/// not the hub's anymore.
const ADDRESS_WAIT: Duration = Duration::from_secs(2);
const DISCOVERY_WAIT: Duration = Duration::from_secs(3);

/// Points dirigent at a plain HTTP test double, e.g. `http://127.0.0.1:8080`, instead of the
/// paired hub's addresses. For tests only.
pub const TEST_HUB: &str = "DIRIGENT_HUB_URL";

/// A connected hub and what it knows.
pub struct Hub {
    pub client: Client,
    pub home: Home,
    pub paired: Paired,
}

impl Hub {
    pub async fn connect(paths: &Paths) -> Result<Self, AppError> {
        let paired = Paired::load(paths)?.ok_or(AppError::NotPaired)?;
        if let Some(address) = test_hub()? {
            let client = Client::plain(address, paired.token())?;
            let home = client.home().await?;
            return Ok(Self {
                client,
                home,
                paired,
            });
        }
        let pin = paired.fingerprint(paths)?;
        let mut tried = Vec::new();
        let mut failure = None;
        if let Some(&address) = paired.addresses.first() {
            match reach(address, pin, &paired).await {
                Ok((client, home)) => {
                    return Ok(Self {
                        client,
                        home,
                        paired,
                    });
                }
                Err(Reach::Refused(error)) => return Err(error),
                Err(Reach::Unreachable(why)) => failure = Some(why),
            }
            tried.push(address);
        }
        // The hub may have a new address: ask the network, then try the older addresses.
        let found = dirigera::find(&paired.uuid, DISCOVERY_WAIT)
            .await
            .ok()
            .flatten()
            .map(|hub| hub.addresses)
            .unwrap_or_default();
        let candidates: Vec<SocketAddr> = found
            .iter()
            .chain(&paired.addresses)
            .filter(|address| !tried.contains(address))
            .copied()
            .collect();
        for address in candidates {
            if tried.contains(&address) {
                // Listed by mDNS and stored both.
                continue;
            }
            tried.push(address);
            match reach(address, pin, &paired).await {
                Ok((client, home)) => {
                    let mut paired = paired;
                    paired.prefer(address, found);
                    paired.save(paths)?;
                    return Ok(Self {
                        client,
                        home,
                        paired,
                    });
                }
                Err(Reach::Refused(error)) => return Err(error),
                Err(Reach::Unreachable(why)) => failure = Some(why),
            }
        }
        Err(AppError::Unreachable(format!(
            "could not reach the hub {} at {}{}",
            paired.name,
            list(&tried),
            failure.map(|why| format!(" ({why})")).unwrap_or_default()
        )))
    }
}

enum Reach {
    /// The hub answered and refused: trying elsewhere will not help.
    Refused(AppError),
    Unreachable(String),
}

async fn reach(
    address: SocketAddr,
    pin: Fingerprint,
    paired: &Paired,
) -> Result<(Client, Home), Reach> {
    let client = Client::new(address, pin, paired.token()).map_err(|e| Reach::Refused(e.into()))?;
    match timeout(ADDRESS_WAIT, client.home()).await {
        Ok(Ok(home)) => Ok((client, home)),
        Ok(Err(error @ (dirigera::Error::Transport(_) | dirigera::Error::Connect(_)))) => {
            Err(Reach::Unreachable(error.to_string()))
        }
        Ok(Err(error)) => Err(Reach::Refused(error.into())),
        Err(_) => Err(Reach::Unreachable(format!(
            "no answer within {} s",
            ADDRESS_WAIT.as_secs()
        ))),
    }
}

fn list(addresses: &[SocketAddr]) -> String {
    if addresses.is_empty() {
        return "no known address".to_owned();
    }
    let all: Vec<String> = addresses.iter().map(ToString::to_string).collect();
    all.join(", ")
}

/// The test double from [`TEST_HUB`], if it is set.
pub fn test_hub() -> Result<Option<SocketAddr>, AppError> {
    let Some(url) = std::env::var_os(TEST_HUB) else {
        return Ok(None);
    };
    let url = url.to_string_lossy();
    url.strip_prefix("http://")
        .and_then(|rest| rest.trim_end_matches('/').parse().ok())
        .map(Some)
        .ok_or_else(|| AppError::Usage(format!("{TEST_HUB} must be http://<ip>:<port>, not {url}")))
}
