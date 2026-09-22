//! Wallet discovery: who bought a token first?
//!
//! Every Pump.fun / PumpSwap / DEX trade references the mint account, so the
//! mint's signature history *is* the token's trade history. We page it back
//! to the launch, take the oldest window of transactions, decode them with
//! "everyone is tracked", and rank the buyers by when they got in.

use std::collections::BTreeMap;

use decode::{DecodedTx, Side};
use serde::Serialize;

use crate::rpc::{RpcClient, RpcError, SignatureInfo};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// 1 = first buyer
    pub rank: u32,
    pub wallet: String,
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    /// slots after the earliest observed transaction on this mint
    pub slots_after_launch: u64,
    pub venue: &'static str,
    pub sol_amount: String,
    pub token_amount: String,
    pub token_decimals: u8,
    /// further buys by this wallet inside the window
    pub extra_buys: u32,
    pub sold_in_window: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub phase: &'static str,
    pub done: u32,
    pub total: u32,
}

/// Result of a discovery run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Discovery {
    pub candidates: Vec<Candidate>,
    /// False when the page budget ran out before the mint's first
    /// transaction — the window is then the oldest slice of what *was*
    /// scanned, not the launch. Raise `max_pages` (or use a faster RPC).
    pub reached_launch: bool,
    /// signatures walked
    pub scanned: u32,
    pub launch_slot: u64,
    /// unix secs of the first transaction in the window
    pub launch_time: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct DiscoverConfig {
    /// `getSignaturesForAddress` pages of 1000 to walk back (caps very
    /// active tokens; 30 pages ≈ 30k transactions).
    pub max_pages: u32,
    /// Oldest successful transactions to fetch and decode.
    pub window: u32,
    pub commitment: String,
}

impl Default for DiscoverConfig {
    fn default() -> Self {
        Self {
            max_pages: 100,
            window: 60,
            commitment: "confirmed".into(),
        }
    }
}

/// Earliest buyers of `mint`, first buyer first.
pub async fn early_buyers(
    rpc: &RpcClient,
    mint: &str,
    cfg: &DiscoverConfig,
    mut progress: impl FnMut(Progress),
) -> Result<Discovery, RpcError> {
    // 1. walk the mint's history back to its beginning
    let mut all: Vec<SignatureInfo> = Vec::new();
    let mut before: Option<String> = None;
    let mut reached_launch = false;
    for page in 0..cfg.max_pages {
        progress(Progress {
            phase: "paging",
            done: page,
            total: cfg.max_pages,
        });
        let batch = rpc
            .get_signatures_for_address(mint, before.as_deref(), None, 1000, &cfg.commitment)
            .await?;
        let n = batch.len();
        before = batch.last().map(|s| s.signature.clone());
        all.extend(batch);
        if n < 1000 {
            reached_launch = true;
            break;
        }
    }
    let scanned = all.len() as u32;
    // newest-first → oldest-first, successful only
    let mut oldest: Vec<SignatureInfo> = all.into_iter().rev().filter(|s| s.err.is_none()).collect();
    oldest.truncate(cfg.window as usize);
    let launch_slot = oldest.first().map(|s| s.slot).unwrap_or(0);
    let launch_time = oldest.first().and_then(|s| s.block_time);

    // 2. fetch + decode the launch window
    let mut by_wallet: BTreeMap<String, Candidate> = BTreeMap::new();
    let total = oldest.len() as u32;
    for (i, s) in oldest.iter().enumerate() {
        progress(Progress {
            phase: "decoding",
            done: i as u32,
            total,
        });
        let Some(tx) = rpc.get_transaction(&s.signature, &cfg.commitment).await? else {
            continue;
        };
        let everyone: Vec<String> = tx.account_keys().iter().map(|k| k.to_string()).collect();
        let DecodedTx::Swaps(swaps) = decode::decode_transaction(&tx, &everyone) else {
            continue;
        };
        for ev in swaps.iter().filter(|e| e.mint == mint) {
            match ev.side {
                Side::Buy => {
                    if let Some(c) = by_wallet.get_mut(&ev.wallet) {
                        c.extra_buys += 1;
                    } else {
                        by_wallet.insert(
                            ev.wallet.clone(),
                            Candidate {
                                rank: 0,
                                wallet: ev.wallet.clone(),
                                signature: ev.signature.clone(),
                                slot: ev.slot,
                                block_time: ev.block_time,
                                slots_after_launch: ev.slot.saturating_sub(launch_slot),
                                venue: ev.venue.as_str(),
                                sol_amount: ev.sol_amount.to_string(),
                                token_amount: ev.token_amount.to_string(),
                                token_decimals: ev.token_decimals,
                                extra_buys: 0,
                                sold_in_window: false,
                            },
                        );
                    }
                }
                Side::Sell => {
                    if let Some(c) = by_wallet.get_mut(&ev.wallet) {
                        c.sold_in_window = true;
                    }
                }
            }
        }
    }
    progress(Progress {
        phase: "done",
        done: total,
        total,
    });

    let mut out: Vec<Candidate> = by_wallet.into_values().collect();
    out.sort_by_key(|c| (c.slot, c.signature.clone()));
    for (i, c) in out.iter_mut().enumerate() {
        c.rank = i as u32 + 1;
    }
    Ok(Discovery {
        candidates: out,
        reached_launch,
        scanned,
        launch_slot,
        launch_time,
    })
}
