//! The paired hub, kept in `hub.json`.

use std::fmt;
use std::net::SocketAddr;

use dirigera::{Fingerprint, Token};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::files;
use crate::paths::Paths;

/// What pairing produced. The file holds the token, so it is readable only by the owner, and
/// `Debug` leaves the token out.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Paired {
    /// The hub's identity, to find it again over mDNS when its address changes.
    pub uuid: String,
    /// The hub's name in the IKEA app at pairing.
    pub name: String,
    /// Where the hub answers, the one that worked last first.
    pub addresses: Vec<SocketAddr>,
    /// SHA-256 of the hub's certificate at pairing. Only a plain HTTP test double has none.
    pub fingerprint: Option<String>,
    pub token: String,
    /// This client among the hub's users, for `dirigent unpair`.
    pub user: Option<String>,
    /// When the pairing was made.
    pub since: Timestamp,
}

impl fmt::Debug for Paired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paired")
            .field("uuid", &self.uuid)
            .field("name", &self.name)
            .field("addresses", &self.addresses)
            .field("fingerprint", &self.fingerprint)
            .field("token", &"<redacted>")
            .field("user", &self.user)
            .field("since", &self.since)
            .finish()
    }
}

impl Paired {
    pub fn load(paths: &Paths) -> Result<Option<Self>, AppError> {
        files::read_json(&paths.hub())
    }

    pub fn save(&self, paths: &Paths) -> Result<(), AppError> {
        files::write_json(&paths.hub(), self)
    }

    /// Forgets the pairing; `false` when there was none.
    pub fn forget(paths: &Paths) -> Result<bool, AppError> {
        files::remove(&paths.hub())
    }

    pub fn token(&self) -> Token {
        Token::new(self.token.clone())
    }

    pub fn fingerprint(&self, paths: &Paths) -> Result<Fingerprint, AppError> {
        let corrupt = |message: &str| AppError::Corrupt {
            path: paths.hub(),
            message: message.to_owned(),
        };
        self.fingerprint
            .as_deref()
            .ok_or_else(|| corrupt("it has no certificate fingerprint"))?
            .parse()
            .map_err(|_| corrupt("its certificate fingerprint is not valid"))
    }

    /// Puts `working` first, so the next command tries it first, and keeps the other known
    /// addresses after it.
    pub fn prefer(&mut self, working: SocketAddr, others: impl IntoIterator<Item = SocketAddr>) {
        let mut addresses = vec![working];
        for address in others.into_iter().chain(self.addresses.iter().copied()) {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        self.addresses = addresses;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paired() -> Paired {
        Paired {
            uuid: "uuid".into(),
            name: "Hub".into(),
            addresses: vec![
                "192.168.1.20:8443".parse().unwrap(),
                "192.168.1.21:8443".parse().unwrap(),
            ],
            fingerprint: Some(Fingerprint::of(b"cert").to_string()),
            token: "SECRET-TOKEN".into(),
            user: Some("user_1".into()),
            since: "2026-01-01T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn the_token_stays_out_of_debug_output() {
        let debug = format!("{:?}", paired());
        assert!(!debug.contains("SECRET-TOKEN"), "{debug}");
    }

    #[test]
    fn the_working_address_goes_first() {
        let mut hub = paired();
        hub.prefer(
            "192.168.1.21:8443".parse().unwrap(),
            ["[fd00::1]:8443".parse().unwrap()],
        );
        let addresses: Vec<String> = hub.addresses.iter().map(ToString::to_string).collect();
        assert_eq!(
            addresses,
            ["192.168.1.21:8443", "[fd00::1]:8443", "192.168.1.20:8443"]
        );
    }

    #[test]
    fn pairing_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        assert_eq!(Paired::load(&paths).unwrap(), None);
        paired().save(&paths).unwrap();
        let loaded = Paired::load(&paths).unwrap().unwrap();
        assert_eq!(loaded, paired());
        assert_eq!(
            loaded.fingerprint(&paths).unwrap(),
            Fingerprint::of(b"cert")
        );
        assert!(Paired::forget(&paths).unwrap());
        assert!(!Paired::forget(&paths).unwrap());
    }
}
