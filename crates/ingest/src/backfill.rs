//! On (re)connect, fetch whatever a wallet did while we weren't listening:
//! page `getSignaturesForAddress` newest-first until we reach the last
//! stored slot (or a signature we already have), then replay oldest-first.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::rpc::{RpcClient, SignatureInfo};
use crate::{ConnState, Cursor, IngestConfig, IngestEvent, RecentSet};

/// Outcome of scanning one page.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PagePlan {
    /// Signatures from this page still to fetch (newest first, as received).
    pub take: Vec<SignatureInfo>,
    /// True once we've crossed into already-known history (or hit a cap).
    pub done: bool,
}

/// Decide which entries of a newest-first page are new. Pure, so it's testable.
///
/// * `last_seen_slot`: the wallet's cursor; entries at or below it are old.
///   `None` means a wallet with no history — take at most `initial_limit`.
/// * `already_taken`: how many we've queued from earlier pages.
/// * `known`: a predicate for "we already processed this signature".
pub(crate) fn plan_page(
    page: &[SignatureInfo],
    last_seen_slot: Option<u64>,
    initial_limit: u32,
    max_total: u32,
    already_taken: u32,
    known: impl Fn(&str) -> bool,
) -> PagePlan {
    let budget = match last_seen_slot {
        None => initial_limit.min(max_total),
        Some(_) => max_total,
    }
    .saturating_sub(already_taken) as usize;

    let mut take = Vec::new();
    for s in page {
        if take.len() >= budget {
            return PagePlan { take, done: true };
        }
        if let Some(last) = last_seen_slot {
            if s.slot <= last {
                return PagePlan { take, done: true };
            }
        }
        if known(&s.signature) {
            return PagePlan { take, done: true };
        }
        take.push(s.clone());
    }
    // A short page means the chain has no more history for this address.
    let done = page.len() < PAGE_SIZE as usize || take.len() >= budget;
    PagePlan { take, done }
}

pub(crate) const PAGE_SIZE: u32 = 100;

pub(crate) async fn backfill_wallet(
    cfg: &IngestConfig,
    rpc: &RpcClient,
    cursor: &dyn Cursor,
    seen: &mut RecentSet,
    wallet: &str,
    out: &mpsc::Sender<IngestEvent>,
    cancel: &CancellationToken,
) {
    let last_seen = cursor.last_seen_slot(wallet).await;
    let mut queued: Vec<SignatureInfo> = Vec::new();
    let mut before: Option<String> = None;

    loop {
        if cancel.is_cancelled() {
            return;
        }
        let page = match rpc
            .get_signatures_for_address(wallet, before.as_deref(), None, PAGE_SIZE, &cfg.commitment)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(wallet, error = %e, "backfill: getSignaturesForAddress failed; skipping wallet");
                return;
            }
        };
        if page.is_empty() {
            break;
        }
        // `known` is async on the cursor; pre-resolve for this page.
        let mut known = std::collections::HashSet::new();
        for s in &page {
            if seen.contains(&s.signature) || cursor.has_signature(&s.signature).await {
                known.insert(s.signature.clone());
                // the first known one ends the scan, no need to look further
                break;
            }
        }
        let plan = plan_page(
            &page,
            last_seen,
            cfg.initial_backfill,
            cfg.max_backfill,
            queued.len() as u32,
            |sig| known.contains(sig),
        );
        before = page.last().map(|s| s.signature.clone());
        queued.extend(plan.take);
        if plan.done {
            break;
        }
    }

    let pending: Vec<SignatureInfo> = queued.into_iter().filter(|s| s.err.is_none()).collect();
    if pending.is_empty() {
        tracing::info!(wallet, ?last_seen, "backfill: nothing new");
        return;
    }
    tracing::info!(wallet, ?last_seen, count = pending.len(), "backfill: fetching");
    let _ = out
        .send(IngestEvent::Status(ConnState::Backfilling {
            wallet: wallet.to_string(),
            pending: pending.len(),
        }))
        .await;
    // oldest first so PnL replays in order
    for s in pending.into_iter().rev() {
        if cancel.is_cancelled() {
            return;
        }
        seen.insert(s.signature.clone());
        crate::fetch_and_forward(cfg, rpc, wallet, &s.signature, false, out).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(slots: &[u64]) -> Vec<SignatureInfo> {
        slots
            .iter()
            .map(|&slot| SignatureInfo {
                signature: format!("sig{slot}"),
                slot,
                err: None,
                block_time: None,
            })
            .collect()
    }

    #[test]
    fn stops_at_last_seen_slot() {
        let p = page(&[110, 109, 105, 100, 99, 90]);
        let plan = plan_page(&p, Some(100), 25, 500, 0, |_| false);
        assert_eq!(plan.take.iter().map(|s| s.slot).collect::<Vec<_>>(), vec![110, 109, 105]);
        assert!(plan.done);
    }

    #[test]
    fn stops_at_known_signature() {
        let p = page(&[110, 109, 105]);
        let plan = plan_page(&p, Some(50), 25, 500, 0, |s| s == "sig109");
        assert_eq!(plan.take.len(), 1);
        assert!(plan.done);
    }

    #[test]
    fn fresh_wallet_takes_initial_limit_only() {
        let p = page(&(0..100).rev().map(|i| 1000 + i).collect::<Vec<_>>());
        let plan = plan_page(&p, None, 25, 500, 0, |_| false);
        assert_eq!(plan.take.len(), 25);
        assert!(plan.done);
    }

    #[test]
    fn full_page_with_no_cutoff_continues() {
        let p = page(&(0..100).rev().map(|i| 1000 + i).collect::<Vec<_>>());
        let plan = plan_page(&p, Some(10), 25, 500, 0, |_| false);
        assert_eq!(plan.take.len(), 100);
        assert!(!plan.done);
        // next page honours the running total against max_total
        let plan2 = plan_page(&p, Some(10), 25, 150, 100, |_| false);
        assert_eq!(plan2.take.len(), 50);
        assert!(plan2.done);
    }

    #[test]
    fn short_page_is_end_of_history() {
        let p = page(&[5, 4, 3]);
        let plan = plan_page(&p, Some(0), 25, 500, 0, |_| false);
        assert_eq!(plan.take.len(), 3);
        assert!(plan.done);
    }
}
