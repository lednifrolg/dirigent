//! Errors from talking to the hub.

use crate::change::ChangeError;
use crate::tls::Fingerprint;

/// A request to the hub failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The hub no longer accepts the token: HTTP 401 for a token it cannot read, e.g. after a
    /// factory reset, and HTTP 403 when this client was removed from the hub's users. Pair
    /// again.
    #[error("the hub no longer accepts this token; pair again")]
    Unauthorized,
    /// The hub presented another certificate than at pairing: after a factory reset, or another
    /// device answers at the hub's address. Pair again.
    #[error("the hub's certificate changed (factory reset?); pair again")]
    CertificateChanged {
        expected: Fingerprint,
        found: Fingerprint,
    },
    /// The hub refused the request (HTTP 4xx), with its explanation if it gave one.
    #[error("the hub refused the request (HTTP {status}){}", message.as_ref().map(|m| format!(": {m}")).unwrap_or_default())]
    Rejected {
        status: u16,
        message: Option<String>,
    },
    /// Any other unsuccessful HTTP status.
    #[error("the hub answered HTTP {status}")]
    Status { status: u16 },
    /// The request did not complete: network, TLS or timeout.
    #[error("could not reach the hub: {0}")]
    Transport(#[source] reqwest::Error),
    /// The event stream could not be opened or broke.
    #[error("the hub's event stream failed: {0}")]
    WebSocket(#[source] tokio_tungstenite::tungstenite::Error),
    /// The connection for the event stream could not be opened.
    #[error("could not connect to the hub: {0}")]
    Connect(#[source] std::io::Error),
    /// The response did not have the expected shape.
    #[error("unexpected response from {endpoint}: {source}")]
    Decode {
        endpoint: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// The change does not fit the device.
    #[error(transparent)]
    Change(#[from] ChangeError),
    /// Looking for hubs on the network failed.
    #[error("mDNS discovery failed: {0}")]
    Discovery(#[from] mdns_sd::Error),
    /// TLS could not be set up.
    #[error("could not set up TLS: {0}")]
    Tls(#[from] rustls::Error),
    /// The operating system could not provide random bytes for pairing.
    #[error("the system random number generator failed: {0}")]
    Random(getrandom::Error),
}
