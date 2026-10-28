//! What the REST client and pairing share: the HTTP client and the mapping of answers to errors.

use std::time::Duration;

use reqwest::{RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::Error;
use crate::tls::Tls;

/// The hub answers on the local network within milliseconds; anything slower is a lost hub.
const TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// The hub closes idle connections after 5 seconds (`Keep-Alive: timeout=5`), so a pooled
/// connection must not wait longer than that.
const POOL_IDLE: Duration = Duration::from_secs(4);
const USER_AGENT: &str = concat!("dirigera/", env!("CARGO_PKG_VERSION"));
/// Longest explanation of a refusal that is kept.
const MESSAGE_MAX: usize = 300;
/// A token whose user was removed from the hub still decodes; the hub then answers 403 with
/// "Provided user id not found in system".
const REMOVED_USER: &str = "user id not found";

pub(crate) fn client(tls: Option<&Tls>) -> Result<reqwest::Client, Error> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .no_proxy()
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE);
    let builder = match tls {
        Some(tls) => builder.tls_backend_preconfigured((*tls.config).clone()),
        None => builder,
    };
    builder.build().map_err(Error::Transport)
}

/// Sends `request` and returns the body of a successful answer.
pub(crate) async fn send(request: RequestBuilder, tls: Option<&Tls>) -> Result<Vec<u8>, Error> {
    let response = request
        .send()
        .await
        .map_err(|error| transport(error, tls))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| transport(error, tls))?;
    if status.is_success() {
        return Ok(body.to_vec());
    }
    Err(refusal(status, &body))
}

pub(crate) fn decode<T: DeserializeOwned>(endpoint: &'static str, body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body).map_err(|source| Error::Decode { endpoint, source })
}

pub(crate) fn refusal(status: StatusCode, body: &[u8]) -> Error {
    let message = message(body);
    let removed = status == StatusCode::FORBIDDEN
        && message
            .as_deref()
            .is_some_and(|m| m.to_lowercase().contains(REMOVED_USER));
    if status == StatusCode::UNAUTHORIZED || removed {
        Error::Unauthorized
    } else if status.is_client_error() {
        Error::Rejected {
            status: status.as_u16(),
            message,
        }
    } else {
        Error::Status {
            status: status.as_u16(),
        }
    }
}

/// A failed request; when the handshake failed on the pin, says so instead.
pub(crate) fn transport(error: reqwest::Error, tls: Option<&Tls>) -> Error {
    match tls.and_then(Tls::mismatch) {
        Some((expected, found)) => Error::CertificateChanged { expected, found },
        None => Error::Transport(error),
    }
}

/// The explanation in an error body: its `message` or `error` field if it is JSON, otherwise
/// the text, without control characters and shortened.
fn message(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let text = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(fields)) => ["message", "error"]
            .iter()
            .find_map(|key| fields.get(*key)?.as_str().map(str::to_owned))
            .unwrap_or_else(|| text.into_owned()),
        _ => text.into_owned(),
    };
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MESSAGE_MAX)
        .collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_keep_the_hub_explanation() {
        let error = refusal(
            StatusCode::BAD_REQUEST,
            br#"{"error":"Bad Request","message":"attributes does not match any of the allowed types"}"#,
        );
        assert!(
            matches!(&error, Error::Rejected { status: 400, message: Some(m) } if m == "attributes does not match any of the allowed types"),
            "{error:?}"
        );
        let error = refusal(StatusCode::FORBIDDEN, b"not\nallowed\x07");
        assert!(
            matches!(&error, Error::Rejected { message: Some(m), .. } if m == "not allowed"),
            "{error:?}"
        );
        assert!(matches!(
            refusal(StatusCode::NOT_FOUND, b""),
            Error::Rejected { message: None, .. }
        ));
        assert!(matches!(
            refusal(StatusCode::UNAUTHORIZED, b"{}"),
            Error::Unauthorized
        ));
        // Both seen live: a token that does not decode, and one whose user was removed.
        assert!(matches!(
            refusal(
                StatusCode::UNAUTHORIZED,
                br#"{"error":"Error","message":"Token could not be decoded"}"#
            ),
            Error::Unauthorized
        ));
        assert!(matches!(
            refusal(
                StatusCode::FORBIDDEN,
                br#"{"error":"Error","message":"Provided user id not found in system: 00000000-0000-4000-8000-000000000000"}"#
            ),
            Error::Unauthorized
        ));
        assert!(matches!(
            refusal(StatusCode::BAD_GATEWAY, b"oops"),
            Error::Status { status: 502 }
        ));
    }
}
