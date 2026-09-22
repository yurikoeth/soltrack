//! The swappable piece: something that tells us "wallet W appeared in
//! signature S". Today that's `logsSubscribe` over WebSocket; a geyser /
//! Yellowstone gRPC implementation would produce the same events.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureNotice {
    pub wallet: String,
    pub signature: String,
    pub slot: u64,
    /// the transaction failed on-chain; nothing to decode
    pub failed: bool,
    /// program log lines, only when the transport was built `with_logs()`
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEvent {
    /// All wallets are subscribed; the caller should backfill now.
    Subscribed,
    Signature(SignatureNotice),
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("connect: {0}")]
    Connect(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("connection closed")]
    Closed,
    #[error("no message for {0:?}; assuming dead connection")]
    Stale(std::time::Duration),
}

#[async_trait]
pub trait Transport: Send {
    /// Connect, subscribe to `wallets`, stream events into `out` until the
    /// connection dies (`Err`) or `cancel` fires (`Ok`). The caller handles
    /// reconnect/backoff and calls `run` again.
    async fn run(
        &mut self,
        wallets: &[String],
        out: mpsc::Sender<TransportEvent>,
        cancel: CancellationToken,
    ) -> Result<(), TransportError>;
}
