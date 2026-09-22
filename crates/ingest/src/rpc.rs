//! Minimal JSON-RPC client for the two Solana methods we need, with the
//! rate limiter in front and `429` / `Retry-After` handling behind.

use std::sync::Arc;
use std::time::Duration;

use decode::RawTransaction;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::ratelimit::RateLimiter;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("http: {0}")]
    Http(reqwest::Error),
    #[error("rpc {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("bad response: {0}")]
    Decode(String),
    #[error("rate limited after {0} attempts")]
    RateLimited(u32),
}

impl From<reqwest::Error> for RpcError {
    /// reqwest's Display appends the request URL, which carries the API key.
    fn from(e: reqwest::Error) -> Self {
        RpcError::Http(e.without_url())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInfo {
    pub signature: String,
    pub slot: u64,
    #[serde(default)]
    pub err: Option<Value>,
    #[serde(default)]
    pub block_time: Option<i64>,
}

#[derive(Debug)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
    limiter: Arc<RateLimiter>,
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcErrorBody>,
}

#[derive(Deserialize)]
struct RpcErrorBody {
    code: i64,
    message: String,
}

impl RpcClient {
    pub fn new(url: impl Into<String>, limiter: Arc<RateLimiter>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("soltrack/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            http,
            url: url.into(),
            limiter,
        }
    }

    pub fn limiter(&self) -> &Arc<RateLimiter> {
        &self.limiter
    }

    /// Raw call. Retries on 429 (honouring `Retry-After`) and on transient
    /// transport errors, a handful of times each.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut rate_limited = 0u32;
        let mut transient = 0u32;
        loop {
            self.limiter.acquire().await;
            let resp = match self.http.post(&self.url).json(&body).send().await {
                Ok(r) => r,
                Err(e) if transient < 3 && (e.is_timeout() || e.is_connect() || e.is_request()) => {
                    transient += 1;
                    tracing::debug!(method, error = %e, "transient http error, retrying");
                    tokio::time::sleep(Duration::from_millis(300 * transient as u64)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if resp.status().as_u16() == 429 {
                rate_limited += 1;
                if rate_limited > 6 {
                    return Err(RpcError::RateLimited(rate_limited));
                }
                let pause = retry_after(&resp).unwrap_or(Duration::from_millis(500 * (1 << rate_limited.min(5))));
                tracing::warn!(method, ?pause, "429 from RPC, backing off");
                self.limiter.penalize(pause).await;
                continue;
            }
            let resp = resp.error_for_status()?;
            let parsed: RpcResponse = resp.json().await?;
            if let Some(e) = parsed.error {
                return Err(RpcError::Rpc {
                    code: e.code,
                    message: e.message,
                });
            }
            return Ok(parsed.result.unwrap_or(Value::Null));
        }
    }

    /// `getTransaction` with `encoding: json`. `Ok(None)` if the node does
    /// not (yet) have it.
    pub async fn get_transaction(
        &self,
        signature: &str,
        commitment: &str,
    ) -> Result<Option<RawTransaction>, RpcError> {
        let v = self
            .call(
                "getTransaction",
                json!([signature, {
                    "encoding": "json",
                    "commitment": commitment,
                    "maxSupportedTransactionVersion": 0
                }]),
            )
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        serde_json::from_value(v)
            .map(Some)
            .map_err(|e| RpcError::Decode(format!("getTransaction {signature}: {e}")))
    }

    /// `getAccountInfo` with base64 data. `Ok(None)` if the account does not
    /// exist. Returns `(owner, data)`.
    pub async fn get_account(&self, pubkey: &str) -> Result<Option<(String, Vec<u8>)>, RpcError> {
        use base64::Engine;
        let v = self
            .call("getAccountInfo", json!([pubkey, {"encoding": "base64"}]))
            .await?;
        let Some(value) = v.get("value").filter(|x| !x.is_null()) else {
            return Ok(None);
        };
        let owner = value
            .get("owner")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::Decode("getAccountInfo: no owner".into()))?
            .to_string();
        let b64 = value
            .pointer("/data/0")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::Decode("getAccountInfo: no data".into()))?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| RpcError::Decode(format!("getAccountInfo base64: {e}")))?;
        Ok(Some((owner, data)))
    }

    /// `getMultipleAccounts` (base64). One entry per requested pubkey; `None`
    /// for accounts that don't exist. Returns `(owner, data)`.
    pub async fn get_multiple_accounts(&self, pubkeys: &[&str]) -> Result<Vec<Option<(String, Vec<u8>)>>, RpcError> {
        use base64::Engine;
        let v = self
            .call("getMultipleAccounts", json!([pubkeys, {"encoding": "base64"}]))
            .await?;
        let values = v
            .get("value")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcError::Decode("getMultipleAccounts: no value".into()))?;
        let mut out = Vec::with_capacity(values.len());
        for value in values {
            if value.is_null() {
                out.push(None);
                continue;
            }
            let owner = value.get("owner").and_then(Value::as_str).unwrap_or_default().to_string();
            let b64 = value.pointer("/data/0").and_then(Value::as_str).unwrap_or_default();
            let data = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| RpcError::Decode(format!("getMultipleAccounts base64: {e}")))?;
            out.push(Some((owner, data)));
        }
        Ok(out)
    }

    /// `getSignaturesForAddress`, newest first.
    pub async fn get_signatures_for_address(
        &self,
        address: &str,
        before: Option<&str>,
        until: Option<&str>,
        limit: u32,
        commitment: &str,
    ) -> Result<Vec<SignatureInfo>, RpcError> {
        let mut opts = json!({"limit": limit.clamp(1, 1000), "commitment": commitment});
        if let Some(b) = before {
            opts["before"] = json!(b);
        }
        if let Some(u) = until {
            opts["until"] = json!(u);
        }
        let v = self
            .call("getSignaturesForAddress", json!([address, opts]))
            .await?;
        serde_json::from_value(v).map_err(|e| RpcError::Decode(format!("getSignaturesForAddress: {e}")))
    }
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let v = resp.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}
