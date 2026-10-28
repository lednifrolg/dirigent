//! The event stream against a fake hub over plain WebSocket.

use std::net::SocketAddr;
use std::time::Duration;

use dirigera::{Client, DeviceId, Error, EventKind, EventStream, Token};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::{WebSocketStream, accept_hdr_async};

const STATE_CHANGED: &str = r#"{"id":"e1","time":"2026-01-01T12:00:00.000Z","specversion":"1.1.0","source":"urn:com:ikea:homesmart:iotc:zigbee","type":"deviceStateChanged","data":{"id":"light_1","type":"light","deviceType":"light","attributes":{"isOn":false},"remoteLinks":[]}}"#;
const PRESSED: &str = r#"{"id":"e2","time":"2026-01-01T12:00:01.000Z","specversion":"1.1.0","source":"urn:com:ikea:homesmart:iotc:zigbee","type":"remotePressEvent","data":{"id":"remote_1","clickPattern":"singlePress"}}"#;

/// Accepts one stream that sends the right token and hands it to `serve`; the address and the
/// Authorization header the client sent.
async fn hub<F, Fut>(serve: F) -> (SocketAddr, oneshot::Receiver<String>)
where
    F: FnOnce(WebSocketStream<tokio::net::TcpStream>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, auth) = oneshot::channel();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut seen = None;
        #[allow(
            clippy::result_large_err,
            reason = "the callback signature is tungstenite's"
        )]
        let check = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
            let header = request
                .headers()
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let ok = header == "Bearer TOKEN" && request.uri().path() == "/v1";
            seen = Some(header);
            if ok {
                Ok(response)
            } else {
                let mut refusal = ErrorResponse::new(None);
                *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                Err(refusal)
            }
        };
        let socket = accept_hdr_async(tcp, check).await;
        let _ = sent.send(seen.unwrap_or_default());
        if let Ok(socket) = socket {
            serve(socket).await;
        }
    });
    (address, auth)
}

fn client(address: SocketAddr, token: &str) -> Client {
    Client::plain(address, Token::new(token)).unwrap()
}

#[tokio::test]
async fn events_arrive_until_the_hub_closes() {
    let (address, auth) = hub(|mut socket| async move {
        socket.send(Message::text(STATE_CHANGED)).await.unwrap();
        socket.send(Message::text(PRESSED)).await.unwrap();
        socket.close(None).await.unwrap();
    })
    .await;
    let mut stream = EventStream::connect(&client(address, "TOKEN"))
        .await
        .unwrap();
    assert_eq!(auth.await.unwrap(), "Bearer TOKEN");

    let changed = stream.next().await.unwrap().unwrap();
    let EventKind::DeviceStateChanged(update) = changed.kind else {
        panic!("{changed:?}");
    };
    assert_eq!(update.id, DeviceId("light_1".into()));
    assert_eq!(update.attributes.is_on(), Some(false));

    let pressed = stream.next().await.unwrap().unwrap();
    assert_eq!(
        pressed.kind,
        EventKind::RemotePress {
            device: DeviceId("remote_1".into()),
            pattern: "singlePress".into()
        }
    );
    assert_eq!(stream.next().await.unwrap(), None);
}

#[tokio::test]
async fn quiet_streams_ping_the_hub() {
    let (pinged, ping) = oneshot::channel();
    let (address, _) = hub(|mut socket| async move {
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            panic!("expected a ping");
        };
        let _ = pinged.send(text.to_string());
        socket.send(Message::text(STATE_CHANGED)).await.unwrap();
        // Keep the stream open until the client is done.
        while socket.next().await.is_some() {}
    })
    .await;
    let client = client(address, "TOKEN");
    let mut stream = EventStream::connect_with(&client, Duration::from_millis(50))
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("the hub answers the ping")
        .unwrap()
        .unwrap();
    assert_eq!(event.event_type, "deviceStateChanged");
    let ping: Value = serde_json::from_str(&ping.await.unwrap()).unwrap();
    assert_eq!(ping["type"], "ping");
    assert_eq!(ping["specversion"], "1.1.0");
    stream.close().await;
}

#[tokio::test]
async fn a_revoked_token_cannot_open_the_stream() {
    let (address, auth) = hub(|_| async {}).await;
    let result = EventStream::connect(&client(address, "OLD")).await;
    assert!(
        matches!(result, Err(Error::Unauthorized)),
        "{:?}",
        result.err()
    );
    assert_eq!(auth.await.unwrap(), "Bearer OLD");
}
