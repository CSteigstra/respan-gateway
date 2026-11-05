use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A key made for these tests alone, of no account.
const KEY: &str = include_str!("test_key.pem");

/// A token endpoint on loopback that answers each call with `token-<n>`,
/// lasting `lifetime` seconds, and hands each call's body to `seen`.
async fn endpoint(lifetime: u64) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!(
        "http://{}/token",
        listener.local_addr().expect("an address")
    );
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (calls, kept) = (Arc::new(AtomicUsize::new(0)), seen.clone());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut raw = Vec::new();
            let mut buf = [0; 8192];
            // Read the head, then as much body as it says.
            loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .and_then(|l| l.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= length {
                        kept.lock().expect("unpoisoned").push(body.to_owned());
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let body = format!(
                r#"{{"access_token":"token-{n}","expires_in":{lifetime},"token_type":"Bearer"}}"#
            );
            let answer = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(answer.as_bytes()).await;
        }
    });
    (at, seen)
}

fn service_account(token_uri: &str) -> String {
    json!({
        "type": "service_account",
        "project_id": "p",
        "private_key_id": "kid-1",
        "private_key": KEY,
        "client_email": "gateway@p.iam.gserviceaccount.com",
        "token_uri": token_uri,
    })
    .to_string()
}

fn form_value(form: &str, name: &str) -> Option<String> {
    let value = form
        .split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")))?;
    Some(
        percent_encoding::percent_decode_str(value)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

#[tokio::test]
async fn a_service_account_signs_a_jwt_for_its_token_and_keeps_it() {
    let (at, seen) = endpoint(3600).await;
    let tokens = GoogleTokens::from_json(&service_account(&at), None, Network::shared())
        .expect("credentials");
    let first = tokens.authorization().await.expect("a token");
    let again = tokens.authorization().await.expect("a token");
    assert_eq!(first.to_str().ok(), Some("Bearer token-1"));
    assert_eq!(again, first, "a token is kept until it nears its expiry");
    let forms = seen.lock().expect("unpoisoned").clone();
    assert_eq!(forms.len(), 1);
    let grant = form_value(&forms[0], "grant_type");
    assert_eq!(
        grant.as_deref(),
        Some("urn:ietf:params:oauth:grant-type:jwt-bearer")
    );
    // The JWT is signed by the account's key, for the token URI.
    let jwt = form_value(&forms[0], "assertion").expect("an assertion");
    let (signed, signature) = jwt.rsplit_once('.').expect("a signature");
    let pair = rsa_key(KEY).expect("the key");
    let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pair.public().as_ref());
    let signature = URL_SAFE_NO_PAD.decode(signature).expect("base64");
    assert!(public.verify(signed.as_bytes(), &signature).is_ok());
    let claims = signed.split('.').nth(1).expect("claims");
    let claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).expect("b64")).expect("JSON");
    assert_eq!(claims["iss"], "gateway@p.iam.gserviceaccount.com");
    assert_eq!(claims["aud"], at);
    assert_eq!(claims["scope"], CLOUD_PLATFORM);
}

#[tokio::test]
async fn a_refused_or_expiring_token_is_minted_anew() {
    let (at, seen) = endpoint(3600).await;
    let tokens = GoogleTokens::from_json(&service_account(&at), None, Network::shared())
        .expect("credentials");
    let _ = tokens.authorization().await.expect("a token");
    tokens.refused();
    let next = tokens.authorization().await.expect("a token");
    assert_eq!(next.to_str().ok(), Some("Bearer token-2"));
    // One that lasts less than the margin is minted on every call.
    let (at, seen_short) = endpoint(60).await;
    let short = GoogleTokens::from_json(&service_account(&at), None, Network::shared())
        .expect("credentials");
    let _ = short.authorization().await.expect("a token");
    let _ = short.authorization().await.expect("a token");
    assert_eq!(seen_short.lock().expect("unpoisoned").len(), 2);
    assert_eq!(seen.lock().expect("unpoisoned").len(), 2);
}

#[tokio::test]
async fn a_users_refresh_token_is_exchanged() {
    let (at, seen) = endpoint(3600).await;
    let user = json!({
        "type": "authorized_user",
        "client_id": "id.apps.googleusercontent.com",
        "client_secret": "s/ecret",
        "refresh_token": "1//refresh",
        "token_uri": at,
    });
    let tokens =
        GoogleTokens::from_json(&user.to_string(), None, Network::shared()).expect("credentials");
    let header = tokens.authorization().await.expect("a token");
    assert_eq!(header.to_str().ok(), Some("Bearer token-1"));
    assert!(header.is_sensitive());
    let form = seen.lock().expect("unpoisoned")[0].clone();
    assert_eq!(
        form_value(&form, "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert_eq!(
        form_value(&form, "client_secret").as_deref(),
        Some("s/ecret")
    );
    assert_eq!(
        form_value(&form, "refresh_token").as_deref(),
        Some("1//refresh")
    );
}

#[test]
fn credentials_it_cannot_read_are_refused_without_their_contents() {
    let read = |json: &str| {
        GoogleTokens::from_json(json, None, Network::shared())
            .err()
            .map(|f| f.message().to_owned())
    };
    assert_eq!(
        read("nope").as_deref(),
        Some("Google credentials: they are not JSON")
    );
    let external = json!({"type": "external_account"}).to_string();
    assert!(read(&external).is_some_and(|m| m.contains("external_account")));
    let bad_key = json!({"type": "service_account", "client_email": "a@b", "private_key": "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----"}).to_string();
    assert_eq!(
        read(&bad_key).as_deref(),
        Some("Google credentials: `private_key` is not a PKCS #8 RSA key")
    );
}
