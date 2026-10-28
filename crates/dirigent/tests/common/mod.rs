//! Helpers shared by the end-to-end tests: the `dirigent` binary run against a fake hub that
//! answers REST requests and streams events on one port, like the real one.
// Each test crate uses a different part of this module.
#![allow(dead_code)]

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

pub const HOME: &str = include_str!("../../../dirigera/tests/data/home.json");
pub const TOKEN: &str = "SYNTHETIC-TOKEN";
pub const STRIP: &str = "a1b2c3d4-0000-4000-8000-000000000001_1";
pub const LAMP: &str = "a1b2c3d4-0000-4000-8000-000000000002_1";

/// How the fake hub answers a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reporting {
    /// Applies it and sends a `deviceStateChanged` event, as the real hub does.
    Events,
    /// Applies it but sends no event: the device never reports back.
    Silent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub body: Value,
}

struct State {
    home: Value,
    requests: Vec<Recorded>,
    removed: bool,
    reporting: Reporting,
    /// Token requests refused before the "button press".
    presses_after: u32,
    token_requests: u32,
    listeners: Vec<mpsc::UnboundedSender<String>>,
}

#[derive(Clone)]
pub struct FakeHub {
    pub address: SocketAddr,
    state: Arc<Mutex<State>>,
}

impl FakeHub {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub = Self {
            address: listener.local_addr().unwrap(),
            state: Arc::new(Mutex::new(State {
                home: serde_json::from_str(HOME).unwrap(),
                requests: Vec::new(),
                removed: false,
                reporting: Reporting::Events,
                presses_after: 0,
                token_requests: 0,
                listeners: Vec::new(),
            })),
        };
        let serving = hub.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let hub = serving.clone();
                tokio::spawn(async move { hub.serve(stream).await });
            }
        });
        hub
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn report(&self, reporting: Reporting) {
        self.state().reporting = reporting;
    }

    /// The token request succeeds only after `refusals` refused ones, as before a button press.
    pub fn press_after(&self, refusals: u32) {
        self.state().presses_after = refusals;
    }

    /// Removes dirigent from the hub's users, as the IKEA app can.
    pub fn remove_user(&self) {
        self.state().removed = true;
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state().requests.clone()
    }

    /// The requests that change something, as `METHOD path body`.
    pub fn changes(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|r| r.method != "GET")
            .map(|r| format!("{} {} {}", r.method, r.path, r.body))
            .collect()
    }

    pub fn listeners(&self) -> usize {
        self.state()
            .listeners
            .iter()
            .filter(|l| !l.is_closed())
            .count()
    }

    /// Sends an event to every open event stream.
    pub fn broadcast(&self, event: &Value) {
        let text = event.to_string();
        self.state()
            .listeners
            .retain(|listener| listener.send(text.clone()).is_ok());
    }

    pub fn device(&self, id: &str) -> Value {
        let state = self.state();
        state.home["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|device| device["id"] == id)
            .cloned()
            .unwrap()
    }

    async fn serve(self, stream: TcpStream) {
        let mut peek = [0; 2048];
        let Ok(n) = stream.peek(&mut peek).await else {
            return;
        };
        let head = String::from_utf8_lossy(&peek[..n]).to_lowercase();
        if head.contains("upgrade: websocket") {
            self.serve_events(stream).await;
        } else {
            self.serve_request(stream).await;
        }
    }

    async fn serve_events(self, stream: TcpStream) {
        let removed = self.state().removed;
        #[allow(
            clippy::result_large_err,
            reason = "the callback signature is tungstenite's"
        )]
        let check =
            move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
                let authorized = request
                    .headers()
                    .get("authorization")
                    .is_some_and(|h| h == format!("Bearer {TOKEN}").as_str());
                match (authorized, removed) {
                    (true, false) => Ok(response),
                    (true, true) => Err(refusal(StatusCode::FORBIDDEN)),
                    (false, _) => Err(refusal(StatusCode::UNAUTHORIZED)),
                }
            };
        let Ok(socket) = accept_hdr_async(stream, check).await else {
            return;
        };
        let (mut sink, mut source) = socket.split();
        let (sender, mut receiver) = mpsc::unbounded_channel::<String>();
        self.state().listeners.push(sender.clone());
        loop {
            tokio::select! {
                outgoing = receiver.recv() => match outgoing {
                    Some(text) => if sink.send(Message::text(text)).await.is_err() { return },
                    None => return,
                },
                incoming = source.next() => match incoming {
                    Some(Ok(Message::Text(text))) if text.contains("\"ping\"") => {
                        let pong = json!({"type": "pong", "data": {"lastModified": "2026-01-01T12:00:00.000Z"}});
                        let _ = sender.send(pong.to_string());
                    }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                    Some(Ok(_)) => {}
                },
            }
        }
    }

    async fn serve_request(self, mut stream: TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        let head_end = loop {
            let Ok(n) = stream.read(&mut buffer).await else {
                return;
            };
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let head = String::from_utf8_lossy(&request[..head_end]).to_string();
        let length: usize = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0);
        while request.len() < head_end + length {
            let Ok(n) = stream.read(&mut buffer).await else {
                return;
            };
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..n]);
        }
        let body = &request[head_end..];
        let mut words = head.split_whitespace();
        let method = words.next().unwrap_or_default().to_owned();
        let target = words.next().unwrap_or_default().to_owned();
        let path = target.split('?').next().unwrap_or_default().to_owned();
        let authorization = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_owned())
        });
        let (status, answer) = self.answer(&method, &path, authorization.as_deref(), body);
        let text = answer.map(|value| value.to_string()).unwrap_or_default();
        let reason = match status {
            200 => "OK",
            202 => "Accepted",
            204 => "No Content",
            401 => "Unauthorized",
            403 => "Forbidden",
            _ => "Not Found",
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    fn answer(
        &self,
        method: &str,
        path: &str,
        authorization: Option<&str>,
        body: &[u8],
    ) -> (u16, Option<Value>) {
        let body: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()))
        };
        let mut state = self.state();
        state.requests.push(Recorded {
            method: method.to_owned(),
            path: path.to_owned(),
            body: body.clone(),
        });
        let parts: Vec<&str> = path.trim_start_matches("/v1/").split('/').collect();
        match (method, parts.as_slice()) {
            ("GET", ["oauth", "authorize"]) => return (200, Some(json!({ "code": "CODE" }))),
            ("POST", ["oauth", "token"]) => {
                state.token_requests += 1;
                return if state.token_requests > state.presses_after {
                    (200, Some(json!({ "access_token": TOKEN })))
                } else {
                    (403, Some(json!({ "error": "Forbidden" })))
                };
            }
            _ => {}
        }
        if authorization != Some(format!("Bearer {TOKEN}").as_str()) {
            return (
                401,
                Some(json!({ "error": "Error", "message": "Token could not be decoded" })),
            );
        }
        if state.removed {
            return (
                403,
                Some(
                    json!({ "error": "Error", "message": "Provided user id not found in system: user" }),
                ),
            );
        }
        match (method, parts.as_slice()) {
            ("GET", ["home"]) => (200, Some(state.home.clone())),
            ("GET", ["users"]) => (200, Some(state.home["users"].clone())),
            ("DELETE", ["users", _]) => {
                state.removed = true;
                (204, None)
            }
            ("GET", ["devices", id]) => {
                let device = state.home["devices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|d| d["id"] == *id)
                    .cloned();
                device.map_or((404, None), |device| (200, Some(device)))
            }
            ("PATCH", ["devices", id]) => {
                let attributes = body[0]["attributes"].clone();
                let id = (*id).to_owned();
                if let Some(device) = state.home["devices"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|d| d["id"] == id.as_str())
                {
                    for (key, value) in attributes.as_object().unwrap() {
                        device["attributes"][key] = value.clone();
                    }
                }
                if state.reporting == Reporting::Events {
                    let event = json!({
                        "id": "event",
                        "time": "2026-01-01T12:00:00.000Z",
                        "specversion": "1.1.0",
                        "source": "urn:test",
                        "type": "deviceStateChanged",
                        "data": { "id": id, "attributes": attributes },
                    })
                    .to_string();
                    state
                        .listeners
                        .retain(|listener| listener.send(event.clone()).is_ok());
                }
                (202, None)
            }
            ("PUT", ["devices", _, "identify"]) | ("POST", ["scenes", _, "trigger" | "undo"]) => {
                (202, None)
            }
            _ => (404, None),
        }
    }
}

fn refusal(status: StatusCode) -> ErrorResponse {
    let mut response = ErrorResponse::new(None);
    *response.status_mut() = status;
    response
}

/// A home directory for dirigent and the hub it talks to.
pub struct Dirigent {
    pub home: tempfile::TempDir,
    pub hub: FakeHub,
}

impl Dirigent {
    pub async fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            hub: FakeHub::start().await,
        }
    }

    /// Paired already, as `dirigent pair` leaves it.
    pub async fn paired() -> Self {
        let dirigent = Self::new().await;
        let hub = json!({
            "uuid": "a1b2c3d4-0000-4000-8000-00000000c0de",
            "name": "Test hub",
            "addresses": [dirigent.hub.address.to_string()],
            "fingerprint": null,
            "token": TOKEN,
            "user": "a1b2c3d4-0000-4000-8000-0000000000f1",
            "since": "2026-01-01T10:00:00Z",
        });
        let state = dirigent.home.path().join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("hub.json"), hub.to_string()).unwrap();
        dirigent
    }

    pub fn file(&self, relative: &str) -> PathBuf {
        self.home.path().join(relative)
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dirigent"));
        command
            .args(args)
            .env("DIRIGENT_HOME", self.home.path())
            .env("DIRIGENT_HUB_URL", format!("http://{}", self.hub.address))
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    pub async fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().await.unwrap()
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}
