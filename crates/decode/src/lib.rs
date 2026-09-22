//! Parse raw Solana transactions (as returned by `getTransaction` with
//! `encoding: "json"`) into normalized [`SwapEvent`]s.
//!
//! Decoders, most specific first:
//! - Pump.fun / PumpSwap: their Anchor `emit_cpi!` events (exact, fee-aware).
//! - Jupiter v6: its `SwapEvent`s, netted across route legs.
//! - Raydium (AMM v4 / CPMM / CLMM), Meteora (DLMM / DAMM v2), Orca Whirlpool:
//!   the SPL token transfers under the swap instruction, reconciled to the
//!   tracked wallet's accounts — one leg out, one leg in, one of them WSOL.
//!
//! Anything else that moves a tracked wallet's tokens is reported as
//! [`DecodedTx::Unknown`] so it can be logged and audited, never guessed at.
//!
//! This crate knows nothing about storage, networking or the UI.

pub mod anchor;
pub mod curve;
pub mod dex;
pub mod event;
pub mod jupiter;
pub mod pda;
pub mod pumpfun;
pub mod pumpswap;
pub mod tx;

pub use event::{DecodedTx, Side, SwapEvent, TokenMeta, UnknownSwap, Venue};
pub use tx::RawTransaction;

use std::collections::BTreeSet;

use tx::FlatIx;

/// Wrapped SOL mint. The SOL leg of every pool we decode.
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// Programs whose presence alone never makes a transaction "a swap".
const BORING_PROGRAMS: &[&str] = &[
    "11111111111111111111111111111111",
    TOKEN_PROGRAM,
    TOKEN_2022_PROGRAM,
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
    "ComputeBudget111111111111111111111111111111",
    "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
    "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo",
];

/// Decode one transaction, keeping only swaps executed by `tracked` wallets.
///
/// Rules, in order:
/// 1. Failed transaction → [`DecodedTx::NotASwap`].
/// 2. Any recognised swap whose trader is a tracked wallet →
///    [`DecodedTx::Swaps`] (all of them; trades by other wallets in the same
///    tx are dropped). Once a router/DEX instruction decodes, the
///    instructions nested under it are not decoded again.
/// 3. Otherwise, if a tracked wallet's non-SOL token balance changed and a
///    non-trivial program was invoked → [`DecodedTx::Unknown`].
/// 4. Otherwise → [`DecodedTx::NotASwap`].
pub fn decode_transaction(tx: &RawTransaction, tracked: &[String]) -> DecodedTx {
    if tx.meta.err.is_some() {
        return DecodedTx::NotASwap;
    }
    let keys = tx.account_keys();
    let flat = tx.flatten_instructions(&keys);
    let signature = tx.signature().to_string();

    let mut swaps = Vec::new();
    // While set, skip everything nested deeper than this stack height.
    let mut skip_below: Option<u32> = None;
    for (pos, ix) in flat.iter().enumerate() {
        if let Some(h) = skip_below {
            if ix.stack_height > h {
                continue;
            }
            skip_below = None;
        }
        let decoded = match Venue::from_program(ix.program) {
            Some(Venue::PumpFunCurve) => find_event(&flat, pos).and_then(|ev| pumpfun::decode(tx, ix, ev)),
            Some(Venue::PumpSwapAmm) => find_event(&flat, pos).and_then(|ev| pumpswap::decode(tx, ix, ev)),
            Some(Venue::Jupiter) => jupiter::decode(tx, &flat, pos, tracked),
            Some(venue) => dex::decode(tx, &flat, pos, venue, tracked),
            None => None,
        };
        if let Some(mut ev) = decoded {
            ev.signature = signature.clone();
            ev.slot = tx.slot;
            ev.block_time = tx.block_time;
            ev.ix_index = ix.ix_index;
            if tracked.iter().any(|w| w == &ev.wallet) {
                swaps.push(ev);
                skip_below = Some(ix.stack_height);
            } else {
                tracing::debug!(sig = %signature, wallet = %ev.wallet, "swap by untracked wallet, dropped");
            }
        }
    }
    // Corroborate against balances: for each (wallet, mint) with swaps on one
    // side only, the wallet's net balance in that mint must have moved the
    // same way. If it didn't, the mint was an intermediate hop through a
    // venue we don't decode (or a router pocketed the proceeds) — booking the
    // partial leg would create a phantom position, so the whole tx is Unknown.
    let deltas = tx.token_deltas(&keys);
    let corroborated = |ev: &SwapEvent| -> bool {
        let same_pair = |o: &SwapEvent| o.wallet == ev.wallet && o.mint == ev.mint;
        let mixed = swaps.iter().any(|o| same_pair(o) && o.side != ev.side);
        if mixed {
            return true; // round-trip within one tx nets to ~0 by design
        }
        let net: i128 = deltas
            .iter()
            .filter(|d| d.owner == ev.wallet && d.mint == ev.mint)
            .map(|d| d.delta)
            .sum();
        match ev.side {
            Side::Buy => net > 0,
            Side::Sell => net < 0,
        }
    };
    let programs: BTreeSet<&str> = flat
        .iter()
        .filter(|ix| ix.stack_height == 1)
        .map(|ix| ix.program)
        .filter(|p| !BORING_PROGRAMS.contains(p))
        .collect();
    let unknown = |wallet: &str| {
        DecodedTx::Unknown(UnknownSwap {
            signature: signature.clone(),
            slot: tx.slot,
            block_time: tx.block_time,
            wallet: wallet.to_string(),
            programs: programs.iter().map(|p| p.to_string()).collect(),
        })
    };
    if let Some(bad) = swaps.iter().find(|ev| !corroborated(ev)) {
        tracing::info!(
            sig = %signature, wallet = %bad.wallet, mint = %bad.mint, venue = bad.venue.as_str(),
            "decoded swap not corroborated by balance change; treating tx as unknown"
        );
        return unknown(&bad.wallet);
    }
    if !swaps.is_empty() {
        return DecodedTx::Swaps(swaps);
    }

    // Rule 3: did a tracked wallet move a token through something non-trivial?
    if programs.is_empty() {
        return DecodedTx::NotASwap;
    }
    for wallet in tracked {
        let moved = deltas
            .iter()
            .any(|d| &d.owner == wallet && d.mint != WSOL_MINT && d.delta != 0);
        if moved {
            return unknown(wallet);
        }
    }
    DecodedTx::NotASwap
}

/// Anchor `emit_cpi!` events arrive as a self-CPI one stack level below the
/// instruction that emitted them. Find that child for the instruction at `pos`.
fn find_event<'a>(flat: &'a [FlatIx<'a>], pos: usize) -> Option<&'a [u8]> {
    let parent = &flat[pos];
    for ix in &flat[pos + 1..] {
        if ix.stack_height <= parent.stack_height {
            break;
        }
        if ix.stack_height == parent.stack_height + 1
            && ix.program == parent.program
            && ix.data.starts_with(&anchor::EVENT_IX_PREFIX)
        {
            return Some(&ix.data[8..]);
        }
    }
    None
}

/// All `emit_cpi!` events (payload after the 8-byte self-CPI prefix) emitted
/// by `program` anywhere under the instruction at `pos`.
pub(crate) fn find_events<'a>(flat: &'a [FlatIx<'a>], pos: usize, program: &str) -> Vec<&'a [u8]> {
    let parent = &flat[pos];
    let mut out = Vec::new();
    for ix in &flat[pos + 1..] {
        if ix.stack_height <= parent.stack_height {
            break;
        }
        if ix.program == program && ix.data.starts_with(&anchor::EVENT_IX_PREFIX) {
            out.push(&ix.data[8..]);
        }
    }
    out
}

/// Children of the instruction at `pos` (everything until the stack unwinds).
pub(crate) fn children<'a>(flat: &'a [FlatIx<'a>], pos: usize) -> &'a [FlatIx<'a>] {
    let parent = &flat[pos];
    let mut end = pos + 1;
    while end < flat.len() && flat[end].stack_height > parent.stack_height {
        end += 1;
    }
    &flat[pos + 1..end]
}
