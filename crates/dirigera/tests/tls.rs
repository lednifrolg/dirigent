//! Certificate pinning against a real TLS server with a throwaway self-signed certificate, the
//! way the hub presents one that names no host.

use std::net::SocketAddr;
use std::sync::Arc;

use dirigera::{Client, Error, EventStream, Fingerprint, Pairing, Token};
use rustls::ServerConfig;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// A TLS server that answers the pairing and home requests, and the fingerprint of its
/// certificate.
async fn hub() -> (SocketAddr, Fingerprint) {
    let certified = rcgen::generate_simple_self_signed(["hub".to_owned()]).unwrap();
    let der = certified.cert.der().clone();
    let fingerprint = Fingerprint::of(&der);
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![der], key)
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // A client that refuses the certificate ends the handshake here.
                let Ok(mut stream) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..n]);
                }
                let head = String::from_utf8_lossy(&request);
                let body = if head.starts_with("GET /v1/oauth/authorize") {
                    r#"{"code":"CODE"}"#
                } else if head.starts_with("GET /v1/home") {
                    r#"{"devices":[],"rooms":[],"scenes":[],"users":[]}"#
                } else {
                    "{}"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            });
        }
    });
    (address, fingerprint)
}

#[tokio::test]
async fn pairing_pins_the_certificate_it_meets() {
    let (address, fingerprint) = hub().await;
    let pairing = Pairing::start(address).await.unwrap();
    assert_eq!(pairing.fingerprint(), Some(fingerprint));
}

#[tokio::test]
async fn the_pinned_certificate_is_accepted() {
    let (address, fingerprint) = hub().await;
    let client = Client::new(address, fingerprint, Token::new("TOKEN")).unwrap();
    let home = client.home().await.unwrap();
    assert_eq!(home.devices, Vec::new());
}

#[tokio::test]
async fn any_other_certificate_is_refused() {
    let (address, fingerprint) = hub().await;
    let pinned = Fingerprint::of(b"the certificate seen at pairing");
    let client = Client::new(address, pinned, Token::new("TOKEN")).unwrap();
    match client.home().await {
        Err(Error::CertificateChanged { expected, found }) => {
            assert_eq!((expected, found), (pinned, fingerprint));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        EventStream::connect(&client).await,
        Err(Error::CertificateChanged { .. })
    ));
}
