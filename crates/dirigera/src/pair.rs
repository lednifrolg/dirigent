//! Pairing: getting a token from the hub with a press of its action button.
//!
//! The hub uses an OAuth authorization code with PKCE: the client asks for a code, the user
//! presses the action button on the bottom of the hub, and the client trades the code for a
//! token. Every pairing adds a user to the hub, see [`crate::Client::users`].

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::ACCEPT;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::tls::{Fingerprint, Tls};
use crate::token::Token;
use crate::{API_PATH, AUDIENCE, http, random};

/// How long a client should keep asking for the token after [`Pairing::start`]: the time the
/// user has to walk to the hub and press the button.
pub const PRESS_WINDOW: Duration = Duration::from_secs(120);

const AUTHORIZE: &str = "/oauth/authorize";
const TOKEN: &str = "/oauth/token";

#[derive(Deserialize)]
struct Authorization {
    code: String,
}

#[derive(Deserialize)]
struct Issued {
    access_token: String,
}

/// A pairing in progress: the hub has handed out a code and waits for the button press.
#[derive(Debug)]
pub struct Pairing {
    http: reqwest::Client,
    base_url: String,
    tls: Option<Tls>,
    code: String,
    verifier: String,
    fingerprint: Option<Fingerprint>,
}

impl Pairing {
    /// Starts pairing with the hub at `address`.
    ///
    /// This is when the hub's certificate is pinned: it is accepted as presented, recorded in
    /// [`Pairing::fingerprint`], and the rest of the pairing must reach the same certificate.
    pub async fn start(address: SocketAddr) -> Result<Self, Error> {
        let base_url = format!("https://{address}{API_PATH}");
        let first_contact = Tls::new(None)?;
        let verifier = code_verifier()?;
        let code = authorize(
            &http::client(Some(&first_contact))?,
            &base_url,
            &verifier,
            Some(&first_contact),
        )
        .await?;
        let fingerprint = first_contact
            .seen()
            .expect("a TLS handshake records the certificate");
        let tls = Tls::new(Some(fingerprint))?;
        Ok(Self {
            http: http::client(Some(&tls))?,
            base_url,
            tls: Some(tls),
            code,
            verifier,
            fingerprint: Some(fingerprint),
        })
    }

    /// Starts pairing with a plain HTTP test double at `address`; there is no certificate.
    pub async fn start_plain(address: SocketAddr) -> Result<Self, Error> {
        let base_url = format!("http://{address}{API_PATH}");
        let http = http::client(None)?;
        let verifier = code_verifier()?;
        let code = authorize(&http, &base_url, &verifier, None).await?;
        Ok(Self {
            http,
            base_url,
            tls: None,
            code,
            verifier,
            fingerprint: None,
        })
    }

    /// The hub's certificate, to pin in [`crate::Client::new`]. `None` only for test doubles.
    pub fn fingerprint(&self) -> Option<Fingerprint> {
        self.fingerprint
    }

    /// Asks once for the token. `Ok(None)` means the button has not been pressed yet: ask again
    /// about once a second, for up to [`PRESS_WINDOW`]. `client_name` is how the hub lists this
    /// client among its users.
    pub async fn finish(&self, client_name: &str) -> Result<Option<Token>, Error> {
        let form = [
            ("code", self.code.as_str()),
            ("name", client_name),
            ("grant_type", "authorization_code"),
            ("code_verifier", self.verifier.as_str()),
        ];
        let request = self
            .http
            .post(format!("{}{TOKEN}", self.base_url))
            .header(ACCEPT, "application/json")
            .form(&form);
        match http::send(request, self.tls.as_ref()).await {
            Ok(body) => {
                let issued: Issued = http::decode("/v1/oauth/token", &body)?;
                Ok(Some(Token::new(issued.access_token)))
            }
            // The hub refuses the code until the button is pressed.
            Err(Error::Rejected { .. } | Error::Unauthorized) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

async fn authorize(
    http: &reqwest::Client,
    base_url: &str,
    verifier: &str,
    tls: Option<&Tls>,
) -> Result<String, Error> {
    let challenge = code_challenge(verifier);
    let query = [
        ("audience", AUDIENCE),
        ("response_type", "code"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    let request = http
        .get(format!("{base_url}{AUTHORIZE}"))
        .header(ACCEPT, "application/json")
        .query(&query);
    let body = http::send(request, tls).await?;
    let authorization: Authorization = http::decode("/v1/oauth/authorize", &body)?;
    Ok(authorization.code)
}

/// A PKCE code verifier: 64 random bytes as unpadded base64url, 86 characters, all of them in
/// the set RFC 7636 allows.
fn code_verifier() -> Result<String, Error> {
    let bytes: [u8; 64] = random::bytes()?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifiers_fit_rfc_7636() {
        let verifier = code_verifier().unwrap();
        assert_eq!(verifier.len(), 86);
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_ne!(verifier, code_verifier().unwrap());
    }

    #[test]
    fn challenge_matches_the_rfc_7636_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
