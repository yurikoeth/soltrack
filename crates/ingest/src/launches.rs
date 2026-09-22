//! Live feed of new Pump.fun tokens.
//!
//! One `logsSubscribe` on the Pump.fun program sees every trade on the
//! platform (dozens per second), but the notification carries the log lines,
//! so we filter for `Instruction: Create` *before* fetching anything. That
//! leaves roughly one transaction fetch every few seconds — fine even on the
//! public RPC. Each fetched transaction is parsed with
//! `decode::pumpfun::find_launch` (the `CreateEvent` plus the dev's buy).

use std::sync::Arc;
use std::time::Duration;

use decode::pumpfun::{Launch as ParsedLaunch, PROGRAM_ID as PUMP_PROGRAM};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::rpc::RpcClient;
use crate::transport::{Transport, TransportEvent};
use crate::ws::WsLogsTransport;
use crate::Backoff;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Launch {
    pub mint: String,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    /// the wallet that created it (and usually made the first buy)
    pub creator: String,
    pub bonding_curve: String,
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    /// the creator's buy in the same transaction, net of fees (0 if none)
    pub dev_buy_lamports: u64,
    pub dev_buy_tokens: u64,
    pub token_total_supply: u64,
    /// price × supply right after launch
    pub initial_mcap_lamports: u128,
}

impl Launch {
    fn from_parsed(p: ParsedLaunch, signature: &str, slot: u64, block_time: Option<i64>) -> Self {
        let mcap = p.create.mcap_lamports();
        Self {
            mint: p.create.mint,
            name: p.create.name,
            symbol: p.create.symbol,
            uri: p.create.uri,
            creator: p.create.creator.unwrap_or_else(|| p.create.user.clone()),
            bonding_curve: p.create.bonding_curve,
            signature: signature.to_string(),
            slot,
            block_time,
            dev_buy_lamports: p.dev_buy_lamports,
            dev_buy_tokens: p.dev_buy_tokens,
            token_total_supply: p.create.token_total_supply,
            initial_mcap_lamports: mcap,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LaunchState {
    Connecting,
    Live,
    Disconnected { reason: String, retry_in_ms: u64 },
}

#[derive(Debug)]
pub enum LaunchEvent {
    Launch(Launch),
    State(LaunchState),
}

/// Is this a token-creation transaction, judging by its log lines?
/// Matches `Instruction: Create` / `CreateV2` / ... but not the associated
/// token program's `CreateIdempotent`, which appears in most swaps.
pub fn looks_like_create(logs: &[String]) -> bool {
    logs.iter().any(|l| {
        let Some(rest) = l.split("Instruction: Create").nth(1) else { return false };
        rest.is_empty() || (rest.starts_with('V') && rest[1..].chars().all(|c| c.is_ascii_digit()))
    })
}

/// Runs until `cancel`; reconnects with backoff.
pub async fn run(
    ws_url: String,
    commitment: String,
    rpc: Arc<RpcClient>,
    budget: Arc<crate::RateLimiter>,
    out: mpsc::Sender<LaunchEvent>,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
    let program = vec![PUMP_PROGRAM.to_string()];
    while !cancel.is_cancelled() {
        let _ = out.send(LaunchEvent::State(LaunchState::Connecting)).await;
        let mut transport = WsLogsTransport::new(&ws_url, &commitment).with_logs();
        let (tx, mut rx) = mpsc::channel::<TransportEvent>(8192);
        let session = cancel.child_token();
        let started = tokio::time::Instant::now();
        let fut = transport.run(&program, tx, session.clone());
        tokio::pin!(fut);

        let reason: String = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break "shutdown".into(),
                res = &mut fut => break match res { Ok(()) => "closed".into(), Err(e) => e.to_string() },
                ev = rx.recv() => match ev {
                    None => break "channel closed".into(),
                    Some(TransportEvent::Subscribed) => {
                        let _ = out.send(LaunchEvent::State(LaunchState::Live)).await;
                    }
                    Some(TransportEvent::Signature(n)) => {
                        if n.failed || !looks_like_create(&n.logs) {
                            continue;
                        }
                        // Launches are best-effort: if we're over budget, drop
                        // this one rather than queue behind wallet traffic.
                        if !budget.try_acquire().await {
                            tracing::debug!(sig = %n.signature, "launch skipped: over budget");
                            continue;
                        }
                        match rpc.get_transaction(&n.signature, &commitment).await {
                            Ok(Some(tx)) => {
                                if let Some(p) = decode::pumpfun::find_launch(&tx) {
                                    let launch = Launch::from_parsed(p, &n.signature, tx.slot, tx.block_time);
                                    tracing::info!(mint = %launch.mint, symbol = %launch.symbol, dev_buy = launch.dev_buy_lamports, "launch");
                                    if out.send(LaunchEvent::Launch(launch)).await.is_err() {
                                        return;
                                    }
                                } else {
                                    tracing::debug!(sig = %n.signature, "create-looking tx without CreateEvent");
                                }
                            }
                            Ok(None) => tracing::debug!(sig = %n.signature, "create tx not available yet; skipped"),
                            Err(e) => tracing::warn!(sig = %n.signature, error = %e, "launch fetch failed"),
                        }
                    }
                },
            }
        };
        session.cancel();
        if cancel.is_cancelled() {
            break;
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff.reset();
        }
        let wait = backoff.next();
        tracing::warn!(%reason, retry_in = ?wait, "launch stream disconnected");
        let _ = out
            .send(LaunchEvent::State(LaunchState::Disconnected {
                reason,
                retry_in_ms: wait.as_millis() as u64,
            }))
            .await;
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_filter() {
        assert!(looks_like_create(&["Program log: Instruction: CreateV2".into()]));
        assert!(looks_like_create(&["Program log: Instruction: Create".into()]));
        assert!(!looks_like_create(&["Program log: Instruction: Buy".into(), "Program log: Instruction: Sell".into()]));
        assert!(!looks_like_create(&["Program log: Instruction: CreateIdempotent".into()]));
        assert!(!looks_like_create(&["Program log: Instruction: CreateAccount".into()]));
    }
}
