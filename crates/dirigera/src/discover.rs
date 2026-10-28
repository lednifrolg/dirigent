//! Finding hubs on the local network over mDNS.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::time::{Instant, timeout_at};

use crate::MDNS_SERVICE;
use crate::error::Error;

/// A hub that announced itself on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// Identity of the hub, stable across addresses and firmware updates.
    pub uuid: String,
    /// mDNS host name, `gw2-` and 12 hex digits.
    pub hostname: String,
    /// Where the API answers: IPv4 first, then IPv6, without link-local addresses.
    pub addresses: Vec<SocketAddr>,
    pub firmware: Option<String>,
    pub api_version: Option<String>,
}

/// Every hub that answers within `wait`.
pub async fn discover(wait: Duration) -> Result<Vec<Discovered>, Error> {
    browse(wait, |_| false).await
}

/// The hub with `uuid`, as soon as it answers, or `None` after `wait`.
pub async fn find(uuid: &str, wait: Duration) -> Result<Option<Discovered>, Error> {
    let found = browse(wait, |hub| hub.uuid == uuid).await?;
    Ok(found.into_iter().find(|hub| hub.uuid == uuid))
}

/// Collects announcements until `wait` ends or `done` accepts a hub.
async fn browse(
    wait: Duration,
    done: impl Fn(&Discovered) -> bool,
) -> Result<Vec<Discovered>, Error> {
    let daemon = ServiceDaemon::new()?;
    let events = daemon.browse(MDNS_SERVICE)?;
    let deadline = Instant::now() + wait;
    let mut hubs: Vec<Discovered> = Vec::new();
    while let Ok(Ok(event)) = timeout_at(deadline, events.recv_async()).await {
        let ServiceEvent::ServiceResolved(service) = event else {
            continue;
        };
        let addresses = service
            .get_addresses()
            .iter()
            .map(mdns_sd::ScopedIp::to_ip_addr);
        let txt = |key: &str| {
            service
                .txt_properties
                .get_property_val_str(key)
                .map(str::to_owned)
        };
        let Some(hub) = announced(service.get_hostname(), service.get_port(), addresses, txt)
        else {
            continue;
        };
        let finished = done(&hub);
        match hubs.iter_mut().find(|known| known.uuid == hub.uuid) {
            Some(known) => *known = merge(known, &hub),
            None => hubs.push(hub),
        }
        if finished {
            break;
        }
    }
    // The daemon runs on its own thread; it stops on its own when shutdown fails.
    let _ = daemon.shutdown();
    Ok(hubs)
}

/// The hub in one announcement, if it has an identity. `txt` reads a TXT record field.
fn announced(
    host: &str,
    port: u16,
    ips: impl IntoIterator<Item = IpAddr>,
    txt: impl Fn(&str) -> Option<String>,
) -> Option<Discovered> {
    let uuid = txt("uuid").filter(|uuid| !uuid.is_empty())?;
    let hostname = txt("hostname").unwrap_or_else(|| {
        host.trim_end_matches('.')
            .trim_end_matches(".local")
            .to_owned()
    });
    // The hub also names an IPv4 address in its TXT record, which may differ from the one
    // its A record resolves to; both answer.
    let listed = txt("ipv4address").and_then(|ip| ip.parse::<IpAddr>().ok());
    let ips: BTreeSet<IpAddr> = ips.into_iter().chain(listed).collect();
    Some(Discovered {
        uuid,
        hostname,
        addresses: ordered(ips, port),
        firmware: txt("sv"),
        api_version: txt("pv"),
    })
}

/// IPv4 before IPv6, without link-local IPv6 addresses, which need an interface to reach.
fn ordered(ips: impl IntoIterator<Item = IpAddr>, port: u16) -> Vec<SocketAddr> {
    let mut usable: Vec<IpAddr> = ips
        .into_iter()
        .filter(|ip| match ip {
            IpAddr::V4(_) => true,
            IpAddr::V6(v6) => !v6.is_unicast_link_local(),
        })
        .collect();
    usable.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    usable.dedup();
    usable
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
}

fn merge(known: &Discovered, newer: &Discovered) -> Discovered {
    let addresses = known
        .addresses
        .iter()
        .chain(&newer.addresses)
        .map(SocketAddr::ip)
        .collect::<BTreeSet<_>>();
    let port = newer
        .addresses
        .first()
        .or(known.addresses.first())
        .map_or(crate::API_PORT, SocketAddr::port);
    Discovered {
        addresses: ordered(addresses, port),
        ..newer.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn txt(fields: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let fields: HashMap<String, String> = fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| fields.get(key).cloned()
    }

    #[test]
    fn announcements_become_hubs() {
        let hub = announced(
            "gw2-0123456789ab.local.",
            8443,
            [
                "fd00::1".parse().unwrap(),
                "fe80::1".parse().unwrap(),
                "192.168.1.20".parse().unwrap(),
            ],
            txt(&[
                ("uuid", "01234567-89ab-4cde-8f01-23456789abcd"),
                ("type", "DIRIGERA"),
                ("pv", "1.6.7"),
                ("sv", "2.975.3"),
                ("hostname", "gw2-0123456789ab"),
                ("ipv4address", "192.168.1.21"),
            ]),
        )
        .unwrap();
        assert_eq!(hub.uuid, "01234567-89ab-4cde-8f01-23456789abcd");
        assert_eq!(hub.hostname, "gw2-0123456789ab");
        assert_eq!(hub.firmware.as_deref(), Some("2.975.3"));
        assert_eq!(hub.api_version.as_deref(), Some("1.6.7"));
        let addresses: Vec<String> = hub.addresses.iter().map(ToString::to_string).collect();
        assert_eq!(
            addresses,
            ["192.168.1.20:8443", "192.168.1.21:8443", "[fd00::1]:8443"]
        );
    }

    #[test]
    fn announcements_without_identity_are_skipped() {
        assert_eq!(announced("x.local.", 8443, [], txt(&[])), None);
        let host_only =
            announced("gw2-0123456789ab.local.", 8443, [], txt(&[("uuid", "u")])).unwrap();
        assert_eq!(host_only.hostname, "gw2-0123456789ab");
        assert_eq!(host_only.addresses, Vec::<SocketAddr>::new());
    }

    #[test]
    fn later_announcements_add_addresses() {
        let first = announced(
            "h.local.",
            8443,
            ["192.168.1.20".parse().unwrap()],
            txt(&[("uuid", "u")]),
        )
        .unwrap();
        let second = announced(
            "h.local.",
            8443,
            ["fd00::1".parse().unwrap()],
            txt(&[("uuid", "u"), ("sv", "2")]),
        )
        .unwrap();
        let merged = merge(&first, &second);
        assert_eq!(merged.addresses.len(), 2);
        assert_eq!(merged.firmware.as_deref(), Some("2"));
    }
}
