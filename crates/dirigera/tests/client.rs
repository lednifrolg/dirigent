//! The REST client and pairing against a mock hub over plain HTTP.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use dirigera::{Change, Client, DeviceId, Error, Home, Pairing, SceneId, Token, UserId};
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HOME: &str = include_str!("data/home.json");
const STRIP: &str = "a1b2c3d4-0000-4000-8000-000000000001_1";
const OUTLET: &str = "a1b2c3d4-0000-4000-8000-000000000002_1";

fn client(server: &MockServer) -> Client {
    Client::plain(*server.address(), Token::new("TOKEN")).unwrap()
}

fn home() -> Home {
    serde_json::from_str(HOME).unwrap()
}

#[tokio::test]
async fn reads_the_home_with_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/home"))
        .and(header("authorization", "Bearer TOKEN"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(HOME, "application/json"))
        .expect(1)
        .mount(&server)
        .await;
    let home = client(&server).home().await.unwrap();
    assert_eq!(home.devices.len(), 4);
}

#[tokio::test]
async fn applies_one_change_per_request() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("/v1/devices/{STRIP}")))
        .and(header("content-type", "application/json"))
        .and(body_json(json!([{ "attributes": { "lightLevel": 40 } }])))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    let home = home();
    let strip = home.device(&DeviceId(STRIP.into())).unwrap();
    client(&server)
        .apply(strip, &Change::Brightness(40))
        .await
        .unwrap();
}

#[tokio::test]
async fn checks_a_change_before_sending_it() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(202))
        .expect(0)
        .mount(&server)
        .await;
    let home = home();
    let outlet = home.device(&DeviceId(OUTLET.into())).unwrap();
    let error = client(&server)
        .apply(outlet, &Change::Brightness(30))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Reading lamp cannot dim");
}

#[tokio::test]
async fn reports_refusals_and_revoked_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "Bad Request",
            "message": "attributes does not match any of the allowed types",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/home"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": "Error",
            "message": "Provided user id not found in system: user_2",
        })))
        .mount(&server)
        .await;
    let client = client(&server);
    let home = home();
    let strip = home.device(&DeviceId(STRIP.into())).unwrap();
    let error = client.apply(strip, &Change::Power(true)).await.unwrap_err();
    assert!(
        matches!(&error, Error::Rejected { status: 400, message: Some(m) } if m.contains("allowed types")),
        "{error:?}"
    );
    assert!(matches!(client.home().await, Err(Error::Unauthorized)));
}

#[tokio::test]
async fn identifies_devices_runs_scenes_and_manages_users() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("/v1/devices/{STRIP}/identify")))
        .and(body_json(json!({ "period": 5 })))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    for action in ["trigger", "undo"] {
        Mock::given(method("POST"))
            .and(path(format!("/v1/scenes/scene_1/{action}")))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "uid": "user_1", "name": "Phone", "audience": "homesmart.local" },
            { "uid": "user_2", "name": "dirigent@test", "audience": "homesmart.local" },
        ])))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/users/user_2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    client
        .identify(&DeviceId(STRIP.into()), Duration::from_secs(5))
        .await
        .unwrap();
    let scene = SceneId("scene_1".into());
    client.trigger_scene(&scene).await.unwrap();
    client.undo_scene(&scene).await.unwrap();
    let users = client.users().await.unwrap();
    assert_eq!(users[1].name, "dirigent@test");
    client.remove_user(&UserId("user_2".into())).await.unwrap();
}

#[tokio::test]
async fn pairing_trades_the_code_once_the_button_is_pressed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/oauth/authorize"))
        .and(query_param("audience", "homesmart.local"))
        .and(query_param("response_type", "code"))
        .and(query_param("code_challenge_method", "S256"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "code": "CODE" })))
        .expect(1)
        .mount(&server)
        .await;
    // Before the press the hub refuses the code.
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "error": "Forbidden" })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "access_token": "NEW-TOKEN" })),
        )
        .mount(&server)
        .await;

    let pairing = Pairing::start_plain(*server.address()).await.unwrap();
    assert_eq!(pairing.fingerprint(), None);
    assert_eq!(pairing.finish("dirigent@test").await.unwrap(), None);
    let token = pairing.finish("dirigent@test").await.unwrap().unwrap();
    assert_eq!(token.expose(), "NEW-TOKEN");

    // The challenge is the SHA-256 of the verifier that the token request reveals.
    let requests = server.received_requests().await.unwrap();
    let challenge = requests[0]
        .url
        .query_pairs()
        .find(|(key, _)| key == "code_challenge")
        .unwrap()
        .1
        .into_owned();
    let form: Vec<(String, String)> = url_form(&requests[2].body);
    let field = |name: &str| {
        form.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(field("code"), Some("CODE"));
    assert_eq!(field("name"), Some("dirigent@test"));
    assert_eq!(field("grant_type"), Some("authorization_code"));
    let verifier = field("code_verifier").unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        challenge
    );
}

#[tokio::test]
async fn pairing_fails_when_the_hub_breaks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/oauth/authorize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "code": "CODE" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let pairing = Pairing::start_plain(*server.address()).await.unwrap();
    assert!(matches!(
        pairing.finish("dirigent@test").await,
        Err(Error::Status { status: 500 })
    ));
}

fn url_form(body: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body).into_owned().collect()
}
