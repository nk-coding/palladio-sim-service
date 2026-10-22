//! Session token verification: RS256 JWTs signed by the backend, checked against the keys it
//! publishes at `<BACKEND_API_URL>/.well-known/jwks.json` (as `JwksPluginTokenVerifier` does in
//! the Kotlin plugin service).

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SCOPE_SESSION_CONNECT: &str = "plugin:session:connect";

#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
    #[serde(rename = "executionId")]
    pub execution_id: Option<String>,
    #[serde(default)]
    pub scope: Vec<String>,
    pub target: Option<String>,
    pub session: Option<String>,
}

pub struct Verifier {
    jwks_url: String,
    issuer: String,
    http: reqwest::Client,
    keys: Mutex<HashMap<String, DecodingKey>>,
    /// When the key set was last fetched. Held while fetching, so sessions opening together
    /// wait for one fetch instead of each being refused for a key that is on its way.
    last_fetch: tokio::sync::Mutex<Option<Instant>>,
}

impl Verifier {
    pub fn new(backend_api_url: &str, issuer: &str) -> Verifier {
        Verifier {
            jwks_url: format!("{}/.well-known/jwks.json", backend_api_url.trim_end_matches('/')),
            issuer: issuer.to_string(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("http client"),
            keys: Mutex::new(HashMap::new()),
            last_fetch: tokio::sync::Mutex::new(None),
        }
    }

    /// The claims of a valid token, or why it is not valid.
    pub async fn verify(&self, token: &str) -> Result<Claims, String> {
        match self.verify_with_held_keys(token).await {
            // The backend may have been restarted with a new key under the same key id (or
            // without one): fetch the published keys again and try once more.
            Err(e) if e.contains("InvalidSignature") && self.refetch().await => self.verify_with_held_keys(token).await,
            other => other,
        }
    }

    async fn verify_with_held_keys(&self, token: &str) -> Result<Claims, String> {
        let header = decode_header(token).map_err(|e| format!("malformed token: {e}"))?;
        let mut v = Validation::new(Algorithm::RS256);
        v.set_issuer(&[&self.issuer]);
        v.leeway = 3;
        v.validate_aud = false;
        let check = |key: &DecodingKey| decode::<Claims>(token, key, &v).map(|d| d.claims);
        match header.kid {
            Some(kid) => {
                let key = match self.key(&kid) {
                    Some(k) => k,
                    None => self.fetch_key(&kid).await?,
                };
                check(&key).map_err(|e| format!("invalid token: {e:?}"))
            }
            // The backend signs without a key id; like auth0's JwkProvider, try the published keys.
            None => {
                if self.keys.lock().unwrap().is_empty() {
                    self.fetch_any().await?;
                }
                let keys: Vec<DecodingKey> = self.keys.lock().unwrap().values().cloned().collect();
                let mut last = "no signing key is published".to_string();
                for key in &keys {
                    match check(key) {
                        Ok(c) => return Ok(c),
                        Err(e) => last = format!("{e:?}"),
                    }
                }
                Err(format!("invalid token: {last}"))
            }
        }
    }

    /// Replaces the held keys with the published ones, at most every ten seconds; whether it did.
    async fn refetch(&self) -> bool {
        let mut last = self.last_fetch.lock().await;
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
            return false;
        }
        *last = Some(Instant::now());
        self.keys.lock().unwrap().clear();
        self.refresh().await.is_ok()
    }

    fn key(&self, kid: &str) -> Option<DecodingKey> {
        self.keys.lock().unwrap().get(kid).cloned()
    }

    /// Fetches the key set when none is held yet.
    async fn fetch_any(&self) -> Result<(), String> {
        let mut last = self.last_fetch.lock().await;
        if !self.keys.lock().unwrap().is_empty() || last.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
            return Ok(());
        }
        *last = Some(Instant::now());
        self.refresh().await
    }

    /// Fetches the key set for a key id it does not know yet, at most every ten seconds.
    async fn fetch_key(&self, kid: &str) -> Result<DecodingKey, String> {
        let mut last = self.last_fetch.lock().await;
        // Another session may have fetched it while this one waited.
        if let Some(k) = self.key(kid) {
            return Ok(k);
        }
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
            return Err(format!("unknown signing key {kid}"));
        }
        *last = Some(Instant::now());
        self.refresh().await?;
        self.key(kid).ok_or_else(|| format!("unknown signing key {kid}"))
    }

    async fn refresh(&self) -> Result<(), String> {
        let set: JwkSet = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("cannot fetch {}: {e}", self.jwks_url))?
            .json()
            .await
            .map_err(|e| format!("cannot read {}: {e}", self.jwks_url))?;
        let mut keys = self.keys.lock().unwrap();
        for jwk in &set.keys {
            if let (Some(kid), Ok(key)) = (&jwk.common.key_id, DecodingKey::from_jwk(jwk)) {
                keys.insert(kid.clone(), key);
            }
        }
        Ok(())
    }
}
