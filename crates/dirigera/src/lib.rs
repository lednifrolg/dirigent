//! Client for the local API of the IKEA DIRIGERA smart home hub.
//!
//! The API is unofficial and undocumented: the IKEA Home smart app uses it on the home network,
//! and this crate follows what the community reverse-engineered from that traffic. Every command
//! switches a real device in a real home, so nothing is retried behind the caller's back. The
//! crate has no UI dependencies; the `dirigent` binary builds the CLI and TUI on top of it.
//!
//! A typical session: [`discover`] the hub, pair once with [`Pairing`] and keep the [`Token`]
//! and the certificate [`Fingerprint`], then [`Client::home`] to read everything, a [`Change`]
//! for [`Client::apply`], and an [`EventStream`] for what changes elsewhere.

mod attributes;
mod change;
mod client;
mod discover;
mod error;
mod events;
mod home;
mod http;
mod pair;
mod random;
mod tls;
mod token;

pub use attributes::{Attributes, ColorMode, TemperatureRange};
pub use change::{Change, ChangeError};
pub use client::Client;
pub use discover::{Discovered, discover, find};
pub use error::Error;
pub use events::{Backoff, DeviceUpdate, Event, EventKind, EventStream, PING_EVERY};
pub use home::{
    Capabilities, Controls, Device, DeviceId, DeviceKind, Home, Room, RoomId, Scene, SceneAction,
    SceneId, SceneInfo, User, UserId,
};
pub use pair::{PRESS_WINDOW, Pairing};
pub use tls::{Fingerprint, ParseFingerprintError};
pub use token::Token;

/// TCP port of the hub's HTTPS API and its WebSocket event stream.
pub const API_PORT: u16 = 8443;

/// Path prefix of every REST endpoint, and the path of the WebSocket event stream.
pub const API_PATH: &str = "/v1";

/// DNS-SD service type the hub announces over mDNS.
pub const MDNS_SERVICE: &str = "_ihsp._tcp.local.";

/// OAuth audience of the tokens the hub issues to clients on the home network.
pub const AUDIENCE: &str = "homesmart.local";
