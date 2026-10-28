//! The hub's WebSocket event stream: what changes, wherever the change came from.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::{Instant, Interval, MissedTickBehavior, interval_at, timeout};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode, header};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{WebSocketStream, client_async};

use crate::attributes::Attributes;
use crate::client::Client;
use crate::error::Error;
use crate::home::{DeviceId, SceneId};
use crate::tls::Tls;
use crate::{API_PATH, random};

/// How often the stream pings the hub. The hub closes a stream that stays quiet, and
/// WebSocket ping frames do not count; an event of type `ping` does.
pub const PING_EVERY: Duration = Duration::from_secs(30);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One message from the hub.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// `deviceStateChanged`, `remotePressEvent`, `sceneTriggered`, …
    pub event_type: String,
    pub time: Option<Timestamp>,
    pub kind: EventKind,
    /// The whole message as the hub sent it.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EventKind {
    /// A device changed; the update carries only what changed.
    DeviceStateChanged(DeviceUpdate),
    /// A remote was pressed. `pattern` is `singlePress`, `doublePress` or `longPress`.
    RemotePress { device: DeviceId, pattern: String },
    /// A scene was triggered, completed, created, updated or deleted; see
    /// [`Event::event_type`].
    Scene(SceneId),
    /// The hub answered a ping.
    Pong,
    /// Any other message.
    Other,
}

/// The changed part of a device, from a `deviceStateChanged` event.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceUpdate {
    pub id: DeviceId,
    #[serde(default)]
    pub attributes: Attributes,
    #[serde(default)]
    pub is_reachable: Option<bool>,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Event {
    /// Reads one message; one that cannot be understood is [`EventKind::Other`].
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        let raw: Value = serde_json::from_str(text)?;
        let event_type = raw["type"].as_str().unwrap_or_default().to_owned();
        let data = raw.get("data").cloned().unwrap_or(Value::Null);
        let id = |data: &Value| data["id"].as_str().map(str::to_owned);
        let kind = match event_type.as_str() {
            "deviceStateChanged" => {
                serde_json::from_value(data).map_or(EventKind::Other, EventKind::DeviceStateChanged)
            }
            "remotePressEvent" => match (id(&data), data["clickPattern"].as_str()) {
                (Some(device), Some(pattern)) => EventKind::RemotePress {
                    device: DeviceId(device),
                    pattern: pattern.to_owned(),
                },
                _ => EventKind::Other,
            },
            scene if scene.starts_with("scene") => {
                id(&data).map_or(EventKind::Other, |scene| EventKind::Scene(SceneId(scene)))
            }
            "pong" => EventKind::Pong,
            _ => EventKind::Other,
        };
        let time = raw["time"].as_str().and_then(|time| time.parse().ok());
        Ok(Self {
            event_type,
            time,
            kind,
            raw,
        })
    }
}

/// An open event stream. Events from while it was not open are lost, so after a (re)connect
/// read the home again.
pub struct EventStream {
    socket: WebSocketStream<Connection>,
    ping: Interval,
}

impl EventStream {
    /// Opens the stream of the hub `client` talks to, with the same certificate pin and token.
    pub async fn connect(client: &Client) -> Result<Self, Error> {
        Self::connect_with(client, PING_EVERY).await
    }

    /// Like [`EventStream::connect`], pinging every `ping_every`.
    pub async fn connect_with(client: &Client, ping_every: Duration) -> Result<Self, Error> {
        let address = client.address();
        let tls = client.tls();
        let scheme = if tls.is_some() { "wss" } else { "ws" };
        let mut request = format!("{scheme}://{address}{API_PATH}")
            .into_client_request()
            .map_err(Error::WebSocket)?;
        let bearer = HeaderValue::from_str(&client.token().bearer())
            .map_err(|error| Error::WebSocket(tungstenite::Error::HttpFormat(error.into())))?;
        request.headers_mut().insert(header::AUTHORIZATION, bearer);
        let connection = Connection::open(address, tls).await?;
        let (socket, _) = client_async(request, connection)
            .await
            .map_err(|error| match error {
                // As over REST: 401 for a token the hub cannot read, 403 for a removed user.
                tungstenite::Error::Http(response)
                    if matches!(
                        response.status(),
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                    ) =>
                {
                    Error::Unauthorized
                }
                other => Error::WebSocket(other),
            })?;
        let mut ping = interval_at(Instant::now() + ping_every, ping_every);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Ok(Self { socket, ping })
    }

    /// Waits for the next event, pinging the hub meanwhile so it keeps the stream open.
    /// `Ok(None)` once the hub closed the stream: wait a [`Backoff`] delay and connect again.
    pub async fn next(&mut self) -> Result<Option<Event>, Error> {
        loop {
            tokio::select! {
                message = self.socket.next() => match message {
                    None | Some(Ok(Message::Close(_))) => return Ok(None),
                    Some(Ok(Message::Text(text))) => {
                        return Event::parse(text.as_str())
                            .map(Some)
                            .map_err(|source| Error::Decode { endpoint: "the event stream", source });
                    }
                    Some(Ok(_)) => {}
                    Some(Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed)) => {
                        return Ok(None);
                    }
                    Some(Err(error)) => return Err(Error::WebSocket(error)),
                },
                _ = self.ping.tick() => {
                    let ping = ping_message()?;
                    self.socket.send(Message::text(ping)).await.map_err(Error::WebSocket)?;
                }
            }
        }
    }

    /// Closes the stream politely.
    pub async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

fn ping_message() -> Result<String, Error> {
    Ok(json!({
        "id": random::uuid()?,
        "specversion": "1.1.0",
        "source": "urn:dirigera",
        "time": Timestamp::now().to_string(),
        "type": "ping",
        "data": null,
    })
    .to_string())
}

/// Delays between reconnects: 1, 2, 4, 8 and 16 seconds, then 30 seconds.
#[derive(Debug, Clone, Default)]
pub struct Backoff {
    attempts: u32,
}

impl Backoff {
    const MAX: Duration = Duration::from_secs(30);

    pub fn next_delay(&mut self) -> Duration {
        let delay = Duration::from_secs(1 << self.attempts.min(5)).min(Self::MAX);
        self.attempts = self.attempts.saturating_add(1);
        delay
    }

    /// Starts over, after a connection held.
    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

/// The socket under the stream: TLS to the hub, or plain TCP to a test double.
enum Connection {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl Connection {
    async fn open(address: SocketAddr, tls: Option<&Tls>) -> Result<Self, Error> {
        let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
            .await
            .map_err(|_| Error::Connect(io::ErrorKind::TimedOut.into()))?
            .map_err(Error::Connect)?;
        let Some(tls) = tls else {
            return Ok(Self::Plain(tcp));
        };
        let name = rustls::pki_types::ServerName::IpAddress(address.ip().into());
        let stream = timeout(
            CONNECT_TIMEOUT,
            TlsConnector::from(tls.config.clone()).connect(name, tcp),
        )
        .await
        .map_err(|_| Error::Connect(io::ErrorKind::TimedOut.into()))?
        .map_err(|error| match tls.mismatch() {
            Some((expected, found)) => Error::CertificateChanged { expected, found },
            None => Error::Connect(error),
        })?;
        Ok(Self::Tls(Box::new(stream)))
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_changes_carry_the_changed_attributes() {
        let event = Event::parse(
            r#"{"id":"e1","time":"2026-01-01T12:00:00.000Z","specversion":"1.1.0","source":"urn:com:ikea:homesmart:iotc:zigbee","type":"deviceStateChanged","data":{"id":"light_1","type":"light","deviceType":"light","attributes":{"isOn":false},"isReachable":true,"lastSeen":"2026-01-01T12:00:00.000Z","remoteLinks":[]}}"#,
        )
        .unwrap();
        assert_eq!(event.event_type, "deviceStateChanged");
        assert_eq!(event.time, Some("2026-01-01T12:00:00Z".parse().unwrap()));
        let EventKind::DeviceStateChanged(update) = event.kind else {
            panic!("{event:?}");
        };
        assert_eq!(update.id, DeviceId("light_1".into()));
        assert_eq!(update.attributes.is_on(), Some(false));
        assert_eq!(update.is_reachable, Some(true));
    }

    #[test]
    fn presses_scenes_and_unknown_messages() {
        let press = Event::parse(
            r#"{"type":"remotePressEvent","data":{"id":"remote_1","clickPattern":"doublePress"}}"#,
        )
        .unwrap();
        assert_eq!(
            press.kind,
            EventKind::RemotePress {
                device: DeviceId("remote_1".into()),
                pattern: "doublePress".into()
            }
        );
        let scene = Event::parse(r#"{"type":"sceneTriggered","data":{"id":"scene_1"}}"#).unwrap();
        assert_eq!(scene.kind, EventKind::Scene(SceneId("scene_1".into())));
        let pong = Event::parse(r#"{"type":"pong","data":null}"#).unwrap();
        assert_eq!(pong.kind, EventKind::Pong);
        let odd = Event::parse(r#"{"type":"deviceStateChanged","data":{"no":"id"}}"#).unwrap();
        assert_eq!(odd.kind, EventKind::Other);
        assert_eq!(odd.raw["data"]["no"], "id");
        assert!(Event::parse("not json").is_err());
    }

    #[test]
    fn pings_look_like_the_app_sends_them() {
        let ping: Value = serde_json::from_str(&ping_message().unwrap()).unwrap();
        assert_eq!(ping["type"], "ping");
        assert_eq!(ping["specversion"], "1.1.0");
        assert_eq!(ping["data"], Value::Null);
        assert_eq!(ping["id"].as_str().unwrap().len(), 36);
        assert!(ping["time"].as_str().unwrap().parse::<Timestamp>().is_ok());
    }

    #[test]
    fn backoff_doubles_up_to_half_a_minute() {
        let mut backoff = Backoff::default();
        let delays: Vec<u64> = (0..8).map(|_| backoff.next_delay().as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }
}
