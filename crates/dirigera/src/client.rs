//! REST client for the hub's local API.

use std::net::SocketAddr;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, RequestBuilder};
use serde_json::{Value, json};

use crate::API_PATH;
use crate::change::Change;
use crate::error::Error;
use crate::home::{Device, DeviceId, Home, SceneId, User, UserId};
use crate::http;
use crate::tls::{Fingerprint, Tls};
use crate::token::Token;

const JSON: &str = "application/json";

/// Client for one paired hub.
///
/// Every call is exactly one HTTP request, and nothing is retried: a command switches a real
/// device, so repeating it is the caller's decision.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    address: SocketAddr,
    tls: Option<Tls>,
    token: Token,
}

impl Client {
    /// A client for the hub at `address` that accepts only the certificate with `fingerprint`,
    /// the one recorded at pairing.
    pub fn new(address: SocketAddr, fingerprint: Fingerprint, token: Token) -> Result<Self, Error> {
        let tls = Tls::new(Some(fingerprint))?;
        Ok(Self {
            http: http::client(Some(&tls))?,
            address,
            tls: Some(tls),
            token,
        })
    }

    /// A client for a plain HTTP test double at `address`.
    pub fn plain(address: SocketAddr, token: Token) -> Result<Self, Error> {
        Ok(Self {
            http: http::client(None)?,
            address,
            tls: None,
            token,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Everything the hub knows (`GET /v1/home`).
    pub async fn home(&self) -> Result<Home, Error> {
        let body = self.send(self.request(Method::GET, "/home")).await?;
        http::decode("/v1/home", &body)
    }

    /// One device as it is now (`GET /v1/devices/{id}`).
    pub async fn device(&self, id: &DeviceId) -> Result<Device, Error> {
        let path = format!("/devices/{id}");
        let body = self.send(self.request(Method::GET, &path)).await?;
        http::decode("/v1/devices/{id}", &body)
    }

    /// Checks `change` against `device` and sends it (`PATCH /v1/devices/{id}`). The hub
    /// accepts it at once; the device shows it a moment later, which a `deviceStateChanged`
    /// event or a new read confirms.
    pub async fn apply(&self, device: &Device, change: &Change) -> Result<(), Error> {
        change.check(device)?;
        let path = format!("/devices/{}", device.id);
        let request = with_json(self.request(Method::PATCH, &path), &change.body());
        self.send(request).await.map(drop)
    }

    /// Makes the device blink for `period` so it can be found; a zero period stops it.
    pub async fn identify(&self, id: &DeviceId, period: Duration) -> Result<(), Error> {
        let path = format!("/devices/{id}/identify");
        let request = with_json(
            self.request(Method::PUT, &path),
            &json!({ "period": period.as_secs() }),
        );
        self.send(request).await.map(drop)
    }

    pub async fn trigger_scene(&self, id: &SceneId) -> Result<(), Error> {
        let path = format!("/scenes/{id}/trigger");
        self.send(self.request(Method::POST, &path)).await.map(drop)
    }

    /// Takes back the latest run of a scene, within its [`crate::Scene::undo_window`].
    pub async fn undo_scene(&self, id: &SceneId) -> Result<(), Error> {
        let path = format!("/scenes/{id}/undo");
        self.send(self.request(Method::POST, &path)).await.map(drop)
    }

    /// Clients paired with the hub (`GET /v1/users`).
    pub async fn users(&self) -> Result<Vec<User>, Error> {
        let body = self.send(self.request(Method::GET, "/users")).await?;
        http::decode("/v1/users", &body)
    }

    /// Unpairs a client; its token stops working.
    pub async fn remove_user(&self, id: &UserId) -> Result<(), Error> {
        let path = format!("/users/{id}");
        self.send(self.request(Method::DELETE, &path))
            .await
            .map(drop)
    }

    pub(crate) fn tls(&self) -> Option<&Tls> {
        self.tls.as_ref()
    }

    pub(crate) fn token(&self) -> &Token {
        &self.token
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let scheme = if self.tls.is_some() { "https" } else { "http" };
        let url = format!("{scheme}://{}{API_PATH}{path}", self.address);
        self.http
            .request(method, url)
            .header(AUTHORIZATION, self.token.bearer())
            .header(ACCEPT, JSON)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Vec<u8>, Error> {
        http::send(request, self.tls.as_ref()).await
    }
}

fn with_json(request: RequestBuilder, body: &Value) -> RequestBuilder {
    request.header(CONTENT_TYPE, JSON).body(body.to_string())
}
