//! Client for the local API of the IKEA DIRIGERA smart home hub.
//!
//! The API is unofficial and undocumented: the IKEA Home smart app uses it on the home network,
//! and this crate follows what the community reverse-engineered from that traffic. Every command
//! switches a real device in a real home. The crate has no UI dependencies; the `dirigent` binary
//! builds the CLI and TUI on top of it.

/// TCP port of the hub's HTTPS API and its WebSocket event stream.
pub const API_PORT: u16 = 8443;

/// Path prefix of every REST endpoint, and the path of the WebSocket event stream.
pub const API_PATH: &str = "/v1";

/// DNS-SD service type the hub announces over mDNS.
pub const MDNS_SERVICE: &str = "_ihsp._tcp.local.";

/// OAuth audience of the tokens the hub issues to clients on the home network.
pub const AUDIENCE: &str = "homesmart.local";
