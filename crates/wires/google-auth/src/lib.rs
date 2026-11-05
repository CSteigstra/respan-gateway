#![warn(missing_docs)]
//! Google access tokens for a transport ([`KeySource`]): from a service
//! account's key, as a signed JWT exchanged at its token URI, or from a
//! user's refresh token, as `gcloud auth application-default login` writes
//! it. A token lasts about an hour; one is kept until five minutes before
//! it expires, or until the provider refuses it, and then minted again, one
//! call minting while the others wait for it.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use http::HeaderMap;
use http::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use respan_gateway_core::Fault;
use respan_gateway_http::Body;
use respan_gateway_http_transport::{HttpTransport, KeyFuture, KeySource, Network, json};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

/// The scope a token is minted for when none is given: every Google Cloud
/// API the credentials may reach, Vertex AI's among them.
pub const CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Where a token is minted when the credentials name no token URI.
const TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// How long before it expires a token is minted anew.
const EARLY: Duration = Duration::from_secs(300);

/// How long a minted JWT asserts it is valid: Google's most.
const ASSERTION_LIFETIME: u64 = 3600;

/// Google access tokens from one set of credentials, kept until they near
/// their expiry ([`KeySource`]).
pub struct GoogleTokens {
    credentials: Credentials,
    scope: String,
    /// Where tokens are minted: the token URI's origin, and its path.
    mint: (HttpTransport, String),
    kept: tokio::sync::Mutex<Option<Kept>>,
}

impl std::fmt::Debug for GoogleTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key and tokens are secrets.
        let kind = match self.credentials {
            Credentials::ServiceAccount { .. } => "service_account",
            Credentials::AuthorizedUser { .. } => "authorized_user",
        };
        f.debug_struct("GoogleTokens")
            .field("type", &kind)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// A token, and when it is minted anew.
struct Kept {
    header: HeaderValue,
    renew_at: Instant,
}

/// What a token is minted from.
enum Credentials {
    /// A service account's key: a JWT it signs is exchanged for a token.
    ServiceAccount {
        email: String,
        key_id: Option<String>,
        key: Box<RsaKeyPair>,
    },
    /// A user's refresh token, exchanged for an access token.
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
}

impl GoogleTokens {
    /// Tokens of the credentials `json`, a Google credentials file's
    /// contents (`"type": "service_account"` or `"authorized_user"`), for
    /// `scope` ([`CLOUD_PLATFORM`] if `None`), minted through `network`.
    /// `Err` with `config` for credentials it cannot read; the error names
    /// no part of them.
    pub fn from_json(json: &str, scope: Option<&str>, network: &Network) -> Result<Self, Fault> {
        let bad = |why: &str| Fault::config(format!("Google credentials: {why}"));
        let v: Value = serde_json::from_str(json).map_err(|_| bad("they are not JSON"))?;
        let field = |name: &str| {
            v[name]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| bad(&format!("`{name}` is missing")))
        };
        let credentials = match v["type"].as_str() {
            Some("service_account") => Credentials::ServiceAccount {
                email: field("client_email")?,
                key_id: v["private_key_id"].as_str().map(str::to_owned),
                key: Box::new(rsa_key(&field("private_key")?).map_err(bad)?),
            },
            Some("authorized_user") => Credentials::AuthorizedUser {
                client_id: field("client_id")?,
                client_secret: field("client_secret")?,
                refresh_token: field("refresh_token")?,
            },
            Some(other) => {
                return Err(bad(&format!(
                    "type `{other}` is not one the gateway mints tokens of: use a service \
                     account's key or `authorized_user` credentials"
                )));
            }
            None => return Err(bad("`type` is missing")),
        };
        let token_uri = v["token_uri"].as_str().unwrap_or(TOKEN_URI);
        Ok(Self {
            credentials,
            scope: scope.unwrap_or(CLOUD_PLATFORM).to_owned(),
            mint: minter(token_uri, network)?,
            kept: tokio::sync::Mutex::new(None),
        })
    }

    /// As a transport's [`KeySource`].
    pub fn into_source(self) -> Arc<dyn KeySource> {
        Arc::new(self)
    }

    /// `Authorization: Bearer <token>`, the token kept if it is not near
    /// its expiry, else minted anew.
    pub async fn authorization(&self) -> Result<HeaderValue, Fault> {
        let mut kept = self.kept.lock().await;
        if let Some(k) = kept.as_ref().filter(|k| Instant::now() < k.renew_at) {
            return Ok(k.header.clone());
        }
        let (token, lifetime) = self.mint().await?;
        let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Fault::upstream("Google's token endpoint answered a malformed token"))?;
        header.set_sensitive(true);
        *kept = Some(Kept {
            header: header.clone(),
            renew_at: Instant::now() + lifetime.saturating_sub(EARLY),
        });
        Ok(header)
    }

    /// A new token and how long it lasts, from Google's token endpoint.
    async fn mint(&self) -> Result<(String, Duration), Fault> {
        let form = match &self.credentials {
            Credentials::ServiceAccount { email, key_id, key } => {
                let assertion = assertion(email, key_id.as_deref(), key, &self.scope, &self.mint)?;
                format!(
                    "grant_type={}&assertion={}",
                    escape("urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    escape(&assertion)
                )
            }
            Credentials::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
            } => format!(
                "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
                escape(client_id),
                escape(client_secret),
                escape(refresh_token)
            ),
        };
        let (transport, path) = &self.mint;
        let call = http::Request::post(path.as_str())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::full(form))
            .map_err(|e| Fault::internal("a token call could not be built").with_source(e))?;
        let refused = |f: Fault| {
            Fault::upstream(format!(
                "Google refused to mint an access token: {}",
                f.message()
            ))
        };
        let (answer, _) = transport.send(call).await.map_err(refused)?;
        let answer = json(answer).await.map_err(refused)?;
        let token = answer["access_token"].as_str().filter(|t| !t.is_empty());
        let token = token
            .ok_or_else(|| Fault::upstream("Google's token endpoint answered no `access_token`"))?;
        let lifetime = Duration::from_secs(answer["expires_in"].as_u64().unwrap_or(3600));
        Ok((token.to_owned(), lifetime))
    }
}

impl KeySource for GoogleTokens {
    fn headers(&self) -> KeyFuture<'_> {
        Box::pin(async move {
            let header = self.authorization().await?;
            Ok(HeaderMap::from_iter([(AUTHORIZATION, header)]))
        })
    }

    fn refused(&self) {
        // A token a provider refuses is minted anew on the next call; a
        // call minting now keeps its own.
        if let Ok(mut kept) = self.kept.try_lock() {
            *kept = None;
        }
    }
}

/// A transport to `token_uri`'s origin, and its path.
fn minter(token_uri: &str, network: &Network) -> Result<(HttpTransport, String), Fault> {
    let uri: http::Uri = token_uri
        .parse()
        .map_err(|_| Fault::config("Google credentials: `token_uri` is not a URL"))?;
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return Err(Fault::config(
            "Google credentials: `token_uri` is not an absolute URL",
        ));
    };
    let transport = HttpTransport::new(format!("{scheme}://{authority}"))?.with_network(network)?;
    Ok((transport, uri.path().to_owned()))
}

/// A service account's PKCS #8 PEM key (`-----BEGIN PRIVATE KEY-----`).
fn rsa_key(pem: &str) -> Result<RsaKeyPair, &'static str> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("-----"))
        .collect();
    let der = STANDARD
        .decode(body)
        .map_err(|_| "`private_key` is not PEM")?;
    RsaKeyPair::from_pkcs8(&der).map_err(|_| "`private_key` is not a PKCS #8 RSA key")
}

/// The JWT a service account signs to ask for a token of `scope`, for the
/// audience of its token URI.
fn assertion(
    email: &str,
    key_id: Option<&str>,
    key: &RsaKeyPair,
    scope: &str,
    (transport, path): &(HttpTransport, String),
) -> Result<String, Fault> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut header = json!({ "alg": "RS256", "typ": "JWT" });
    if let Some(kid) = key_id {
        header["kid"] = kid.into();
    }
    let claims = json!({
        "iss": email,
        "scope": scope,
        "aud": format!("{}{path}", transport.base()),
        "iat": now,
        "exp": now + ASSERTION_LIFETIME,
    });
    let part = |v: &Value| URL_SAFE_NO_PAD.encode(v.to_string());
    let signed = format!("{}.{}", part(&header), part(&claims));
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .map_err(|_| Fault::internal("a service account's JWT could not be signed"))?;
    Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// `text` escaped as a form value.
fn escape(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests;
