//! `logsSubscribe` over the standard Solana WebSocket RPC. One subscription
//! per wallet (`mentions` accepts exactly one address).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::transport::{SignatureNotice, Transport, TransportError, TransportEvent};

pub struct WsLogsTransport {
    url: String,
    commitment: String,
    /// forward log lines in notices (for filters that must run before fetching)
    keep_logs: bool,
    /// Send a ping if nothing arrived for this long…
    ping_after: Duration,
    /// …and give up if still nothing after this long.
    stale_after: Duration,
}

impl WsLogsTransport {
    pub fn new(url: impl Into<String>, commitment: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            commitment: commitment.into(),
            keep_logs: false,
            ping_after: Duration::from_secs(30),
            stale_after: Duration::from_secs(90),
        }
    }

    pub fn with_logs(mut self) -> Self {
        self.keep_logs = true;
        self
    }
}

/// What one inbound frame means to us.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Inbound {
    /// `{"id": n, "result": subscription_id}`
    SubscribeAck { request_id: u64, subscription: u64 },
    /// `{"id": n, "error": {...}}`
    SubscribeErr { request_id: u64, message: String },
    Notification { subscription: u64, slot: u64, signature: String, failed: bool, logs: Vec<String> },
    Other,
}

pub(crate) fn parse_inbound(text: &str) -> Result<Inbound, TransportError> {
    let v: Value = serde_json::from_str(text).map_err(|e| TransportError::Protocol(e.to_string()))?;
    if let Some(id) = v.get("id").and_then(Value::as_u64) {
        if let Some(sub) = v.get("result").and_then(Value::as_u64) {
            return Ok(Inbound::SubscribeAck {
                request_id: id,
                subscription: sub,
            });
        }
        if let Some(err) = v.get("error") {
            return Ok(Inbound::SubscribeErr {
                request_id: id,
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            });
        }
        return Ok(Inbound::Other);
    }
    if v.get("method").and_then(Value::as_str) == Some("logsNotification") {
        let params = v.get("params").ok_or_else(|| TransportError::Protocol("no params".into()))?;
        let subscription = params
            .get("subscription")
            .and_then(Value::as_u64)
            .ok_or_else(|| TransportError::Protocol("no subscription id".into()))?;
        let result = params.get("result").ok_or_else(|| TransportError::Protocol("no result".into()))?;
        let slot = result
            .pointer("/context/slot")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let value = result.get("value").ok_or_else(|| TransportError::Protocol("no value".into()))?;
        let signature = value
            .get("signature")
            .and_then(Value::as_str)
            .ok_or_else(|| TransportError::Protocol("no signature".into()))?
            .to_string();
        let failed = value.get("err").map_or(false, |e| !e.is_null());
        let logs = value
            .get("logs")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        return Ok(Inbound::Notification {
            subscription,
            slot,
            signature,
            failed,
            logs,
        });
    }
    Ok(Inbound::Other)
}

#[async_trait]
impl Transport for WsLogsTransport {
    async fn run(
        &mut self,
        wallets: &[String],
        out: mpsc::Sender<TransportEvent>,
        cancel: CancellationToken,
    ) -> Result<(), TransportError> {
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(20),
            tokio_tungstenite::connect_async(&self.url),
        )
        .await
        .map_err(|_| TransportError::Connect("timeout".into()))?
        .map_err(|e| TransportError::Connect(e.to_string()))?;
        let (mut sink, mut stream) = ws.split();
        tracing::info!(host = %crate::redact_url(&self.url), wallets = wallets.len(), "ws connected");

        // request id → wallet, then subscription id → wallet
        let mut pending: HashMap<u64, String> = HashMap::new();
        let mut subs: HashMap<u64, String> = HashMap::new();
        for (i, w) in wallets.iter().enumerate() {
            let id = i as u64 + 1;
            let req = json!({
                "jsonrpc": "2.0", "id": id, "method": "logsSubscribe",
                "params": [{"mentions": [w]}, {"commitment": self.commitment}]
            });
            sink.send(Message::Text(req.to_string().into()))
                .await
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            pending.insert(id, w.clone());
        }
        if wallets.is_empty() {
            let _ = out.send(TransportEvent::Subscribed).await;
        }

        let mut last_rx = tokio::time::Instant::now();
        let mut pinged = false;
        loop {
            let idle = last_rx.elapsed();
            if idle >= self.stale_after {
                return Err(TransportError::Stale(idle));
            }
            let next_check = if pinged { self.stale_after } else { self.ping_after }.saturating_sub(idle);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(());
                }
                msg = stream.next() => {
                    last_rx = tokio::time::Instant::now();
                    pinged = false;
                    let msg = match msg {
                        None => return Err(TransportError::Closed),
                        Some(Err(e)) => return Err(TransportError::Protocol(e.to_string())),
                        Some(Ok(m)) => m,
                    };
                    match msg {
                        Message::Text(text) => match parse_inbound(&text)? {
                            Inbound::SubscribeAck { request_id, subscription } => {
                                if let Some(w) = pending.remove(&request_id) {
                                    tracing::debug!(wallet = %w, subscription, "subscribed");
                                    subs.insert(subscription, w);
                                    if pending.is_empty() {
                                        let _ = out.send(TransportEvent::Subscribed).await;
                                    }
                                }
                            }
                            Inbound::SubscribeErr { request_id, message } => {
                                let w = pending.remove(&request_id).unwrap_or_default();
                                return Err(TransportError::Protocol(format!("logsSubscribe {w}: {message}")));
                            }
                            Inbound::Notification { subscription, slot, signature, failed, logs } => {
                                if let Some(w) = subs.get(&subscription) {
                                    let logs = if self.keep_logs { logs } else { Vec::new() };
                                    let n = SignatureNotice { wallet: w.clone(), signature, slot, failed, logs };
                                    if out.send(TransportEvent::Signature(n)).await.is_err() {
                                        return Ok(());
                                    }
                                }
                            }
                            Inbound::Other => {}
                        },
                        Message::Ping(p) => {
                            let _ = sink.send(Message::Pong(p)).await;
                        }
                        Message::Close(_) => return Err(TransportError::Closed),
                        _ => {}
                    }
                }
                _ = tokio::time::sleep(next_check) => {
                    if !pinged {
                        pinged = true;
                        sink.send(Message::Ping(Vec::new().into()))
                            .await
                            .map_err(|e| TransportError::Protocol(e.to_string()))?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subscribe_ack() {
        let m = parse_inbound(r#"{"jsonrpc":"2.0","result":24040,"id":1}"#).unwrap();
        assert_eq!(m, Inbound::SubscribeAck { request_id: 1, subscription: 24040 });
    }

    #[test]
    fn parses_notification() {
        let text = r#"{"jsonrpc":"2.0","method":"logsNotification","params":{"result":{"context":{"slot":5208469},"value":{"signature":"5h6xBEauJ3PK6SWCZ1PGjBvj8vDdWG3KpwATGy1ARAXFSDwt8GFXM7W5Ncn16wmqokgpiKRLuS83KUxyZyv2sUYv","err":null,"logs":["Program 11111111111111111111111111111111 invoke [1]"]}},"subscription":24040}}"#;
        let m = parse_inbound(text).unwrap();
        assert_eq!(
            m,
            Inbound::Notification {
                subscription: 24040,
                slot: 5208469,
                signature: "5h6xBEauJ3PK6SWCZ1PGjBvj8vDdWG3KpwATGy1ARAXFSDwt8GFXM7W5Ncn16wmqokgpiKRLuS83KUxyZyv2sUYv".into(),
                failed: false,
                logs: vec!["Program 11111111111111111111111111111111 invoke [1]".into()],
            }
        );
    }

    #[test]
    fn failed_tx_is_flagged() {
        let text = r#"{"jsonrpc":"2.0","method":"logsNotification","params":{"result":{"context":{"slot":1},"value":{"signature":"sig","err":{"InstructionError":[3,{"Custom":3}]},"logs":[]}},"subscription":7}}"#;
        match parse_inbound(text).unwrap() {
            Inbound::Notification { failed, .. } => assert!(failed),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn subscribe_error_and_junk() {
        let m = parse_inbound(r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"Invalid params"},"id":2}"#).unwrap();
        assert_eq!(m, Inbound::SubscribeErr { request_id: 2, message: "Invalid params".into() });
        assert_eq!(parse_inbound(r#"{"jsonrpc":"2.0","method":"slotNotification","params":{}}"#).unwrap(), Inbound::Other);
        assert!(parse_inbound("not json").is_err());
    }
}
