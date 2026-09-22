//! Realtime ingestion of tracked wallets' transactions.
//!
//! ```text
//!   Transport (WS logsSubscribe today, geyser later)
//!       │  SignatureNotice
//!       ▼
//!   Ingest loop ── dedupe (Cursor + recent set) ── RpcClient.get_transaction
//!       │                                              ▲ RateLimiter
//!       ▼  IngestEvent::Transaction                    │
//!   caller (decode → store → pnl → UI)                 │
//!       └── on (re)connect: backfill via getSignaturesForAddress ──┘
//! ```
//!
//! This crate fetches and forwards raw transactions; it never decodes them.

/// Scheme + host only: provider URLs carry the API key in the path or query.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host = rest.split(|c| c == '/' || c == '?' || c == '#').next().unwrap_or(rest);
    if scheme.is_empty() { host.to_string() } else { format!("{scheme}://{host}") }
}

pub mod backfill;
pub mod discover;
pub mod launches;
pub mod metadata;
pub mod tokens;
pub mod ratelimit;
pub mod rpc;
pub mod transport;
pub mod ws;

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use decode::RawTransaction;
use tokio::sync::{mpsc, Notify, RwLock};
use tokio_util::sync::CancellationToken;

pub use ratelimit::{RateLimitConfig, RateLimiter};
pub use rpc::{RpcClient, RpcError};
pub use transport::{SignatureNotice, Transport, TransportEvent};
pub use ws::WsLogsTransport;
pub use discover::{Candidate, DiscoverConfig, Discovery, Progress as DiscoverProgress};
pub use launches::{Launch, LaunchEvent, LaunchState};
pub use tokens::{CurveState, PoolState, TokenState};

#[derive(Debug, Clone)]
pub struct IngestConfig {
    pub rpc_url: String,
    pub ws_url: String,
    /// Signatures to pull for a wallet with no stored history.
    pub initial_backfill: u32,
    /// Hard cap on signatures fetched per wallet per (re)connect.
    pub max_backfill: u32,
    pub rate: RateLimitConfig,
    /// `confirmed` (default) or `finalized`
    pub commitment: String,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            rpc_url: "https://api.mainnet-beta.solana.com".into(),
            ws_url: "wss://api.mainnet-beta.solana.com".into(),
            initial_backfill: 25,
            max_backfill: 500,
            rate: RateLimitConfig::default(),
            commitment: "confirmed".into(),
        }
    }
}

/// The live set of tracked wallets. Changing it makes the ingest loop drop
/// its subscriptions and reconnect with the new list (backfill covers the
/// history of anything added).
#[derive(Debug, Default)]
pub struct WalletSet {
    inner: RwLock<Vec<String>>,
    changed: Notify,
}

impl WalletSet {
    pub fn new(wallets: Vec<String>) -> Self {
        Self {
            inner: RwLock::new(wallets),
            changed: Notify::new(),
        }
    }

    pub async fn get(&self) -> Vec<String> {
        self.inner.read().await.clone()
    }

    pub fn get_blocking(&self) -> Vec<String> {
        self.inner.blocking_read().clone()
    }

    /// Returns false if already present.
    pub async fn add(&self, wallet: &str) -> bool {
        let mut w = self.inner.write().await;
        if w.iter().any(|x| x == wallet) {
            return false;
        }
        w.push(wallet.to_string());
        drop(w);
        self.changed.notify_one();
        true
    }

    /// Returns false if it wasn't tracked.
    pub async fn remove(&self, wallet: &str) -> bool {
        let mut w = self.inner.write().await;
        let before = w.len();
        w.retain(|x| x != wallet);
        let removed = w.len() != before;
        drop(w);
        if removed {
            self.changed.notify_one();
        }
        removed
    }
}

/// What the ingest loop needs to know about already-processed history.
/// Implemented by the app on top of its `Storage`.
#[async_trait]
pub trait Cursor: Send + Sync {
    async fn has_signature(&self, signature: &str) -> bool;
    async fn last_seen_slot(&self, wallet: &str) -> Option<u64>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnState {
    /// nothing to track yet
    Idle,
    Connecting,
    Subscribed { wallets: usize },
    Backfilling { wallet: String, pending: usize },
    Live,
    Disconnected { reason: String, retry_in_ms: u64 },
}

#[derive(Debug)]
pub enum IngestEvent {
    /// A transaction that mentions `wallet`, fetched in full. Order is not
    /// guaranteed across wallets; within a backfill it is oldest-first.
    /// `live` is false for backfilled history — alerts should ignore those.
    Transaction {
        wallet: String,
        tx: RawTransaction,
        live: bool,
    },
    Status(ConnState),
    /// Something was fetched but couldn't be forwarded (logged; UI may show a counter).
    FetchFailed { wallet: String, signature: String, error: String },
}

/// Drives one transport for the life of the app, reconnecting with backoff.
pub async fn run(
    cfg: IngestConfig,
    mut transport: Box<dyn Transport>,
    rpc: Arc<RpcClient>,
    cursor: Arc<dyn Cursor>,
    wallet_set: Arc<WalletSet>,
    out: mpsc::Sender<IngestEvent>,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
    let mut seen = RecentSet::new(20_000);

    while !cancel.is_cancelled() {
        let wallets = wallet_set.get().await;
        if wallets.is_empty() {
            let _ = out.send(IngestEvent::Status(ConnState::Idle)).await;
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = wallet_set.changed.notified() => continue,
            }
        }
        let _ = out.send(IngestEvent::Status(ConnState::Connecting)).await;
        let (notice_tx, mut notice_rx) = mpsc::channel::<TransportEvent>(4096);
        let session_cancel = cancel.child_token();
        let started = tokio::time::Instant::now();
        let mut resubscribe = false;

        let transport_fut = transport.run(&wallets, notice_tx, session_cancel.clone());
        tokio::pin!(transport_fut);

        let reason: String = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break "shutdown".into(),
                _ = wallet_set.changed.notified() => {
                    resubscribe = true;
                    break "wallet list changed".into();
                }
                res = &mut transport_fut => {
                    break match res {
                        Ok(()) => "transport closed".into(),
                        Err(e) => e.to_string(),
                    };
                }
                ev = notice_rx.recv() => {
                    match ev {
                        None => break "transport channel closed".into(),
                        Some(TransportEvent::Subscribed) => {
                            let _ = out.send(IngestEvent::Status(ConnState::Subscribed { wallets: wallets.len() })).await;
                            for w in &wallets {
                                backfill::backfill_wallet(&cfg, &rpc, cursor.as_ref(), &mut seen, w, &out, &cancel).await;
                            }
                            let _ = out.send(IngestEvent::Status(ConnState::Live)).await;
                        }
                        Some(TransportEvent::Signature(n)) => {
                            handle_notice(&cfg, &rpc, cursor.as_ref(), &mut seen, n, &out).await;
                        }
                    }
                }
            }
        };
        session_cancel.cancel();
        if cancel.is_cancelled() {
            break;
        }
        if resubscribe {
            tracing::info!("wallet list changed; resubscribing");
            backoff.reset();
            continue;
        }
        // A session that held for a while earns a fresh backoff schedule.
        if started.elapsed() > Duration::from_secs(60) {
            backoff.reset();
        }
        let wait = backoff.next();
        tracing::warn!(%reason, retry_in = ?wait, "ingest transport disconnected");
        let _ = out
            .send(IngestEvent::Status(ConnState::Disconnected {
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

async fn handle_notice(
    cfg: &IngestConfig,
    rpc: &RpcClient,
    cursor: &dyn Cursor,
    seen: &mut RecentSet,
    n: SignatureNotice,
    out: &mpsc::Sender<IngestEvent>,
) {
    if n.failed {
        tracing::debug!(sig = %n.signature, "skipping failed tx");
        return;
    }
    if seen.contains(&n.signature) || cursor.has_signature(&n.signature).await {
        return;
    }
    seen.insert(n.signature.clone());
    fetch_and_forward(cfg, rpc, &n.wallet, &n.signature, true, out).await;
}

/// Fetch one transaction and forward it. A freshly confirmed tx can lag the
/// WS notification by a moment, so a `null` result is retried briefly.
pub(crate) async fn fetch_and_forward(
    cfg: &IngestConfig,
    rpc: &RpcClient,
    wallet: &str,
    signature: &str,
    live: bool,
    out: &mpsc::Sender<IngestEvent>,
) {
    let mut attempt = 0u32;
    loop {
        match rpc.get_transaction(signature, &cfg.commitment).await {
            Ok(Some(tx)) => {
                let _ = out
                    .send(IngestEvent::Transaction {
                        wallet: wallet.to_string(),
                        tx,
                        live,
                    })
                    .await;
                return;
            }
            Ok(None) if attempt < 5 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
            }
            Ok(None) => {
                tracing::warn!(sig = signature, "transaction not available after retries");
                let _ = out
                    .send(IngestEvent::FetchFailed {
                        wallet: wallet.to_string(),
                        signature: signature.to_string(),
                        error: "not found".into(),
                    })
                    .await;
                return;
            }
            Err(e) => {
                tracing::warn!(sig = signature, error = %e, "getTransaction failed");
                let _ = out
                    .send(IngestEvent::FetchFailed {
                        wallet: wallet.to_string(),
                        signature: signature.to_string(),
                        error: e.to_string(),
                    })
                    .await;
                return;
            }
        }
    }
}

/// Bounded set of recently handled signatures (FIFO eviction).
pub(crate) struct RecentSet {
    set: HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl RecentSet {
    pub fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    pub fn contains(&self, s: &str) -> bool {
        self.set.contains(s)
    }

    pub fn insert(&mut self, s: String) {
        if self.set.insert(s.clone()) {
            self.order.push_back(s);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.set.remove(&old);
                }
            }
        }
    }
}

/// Exponential backoff with deterministic-but-spread jitter (no rand dep).
pub(crate) struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempt: 0,
        }
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn next(&mut self) -> Duration {
        let exp = self.base.saturating_mul(1u32 << self.attempt.min(16));
        let capped = exp.min(self.max);
        self.attempt = self.attempt.saturating_add(1);
        // ±25% jitter from the clock's low bits
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let jitter_pct = (nanos % 51) as i64 - 25; // -25..=25
        let ms = capped.as_millis() as i64;
        let jittered = ms + ms * jitter_pct / 100;
        Duration::from_millis(jittered.max(1) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_set_evicts_oldest() {
        let mut s = RecentSet::new(2);
        s.insert("a".into());
        s.insert("b".into());
        s.insert("c".into());
        assert!(!s.contains("a"));
        assert!(s.contains("b") && s.contains("c"));
        s.insert("b".into()); // duplicate insert doesn't grow
        assert!(s.contains("c"));
    }

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        let mut last = Duration::ZERO;
        for _ in 0..10 {
            let d = b.next();
            assert!(d <= Duration::from_secs(75)); // 60s + 25% jitter
            last = d;
        }
        assert!(last >= Duration::from_secs(45)); // 60s − 25% jitter
        b.reset();
        assert!(b.next() <= Duration::from_millis(1250));
    }
}
