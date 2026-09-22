//! Generic decoder for AMMs without a usable event: Raydium (AMM v4, CPMM,
//! CLMM), Meteora (DLMM, DAMM v2), Orca Whirlpool.
//!
//! A swap instruction on any of these moves tokens with exactly two SPL
//! transfers that touch the user: one *out* of a user-owned account into a
//! pool vault, one *in* from a vault to a user-owned account. We read those
//! transfers (amounts are exact), resolve account owners/mints from the
//! transaction's token-balance metadata, and require:
//!
//! * exactly one outgoing and one incoming user leg,
//! * one leg is WSOL, the other a single token mint,
//! * the token mint's decimals are known.
//!
//! Anything else returns `None` and the transaction falls through to
//! `UnknownSwap`. A temporary WSOL account that is created and closed inside
//! the same transaction appears in neither pre nor post balances, so its
//! owner/mint are unresolvable; the *only* inference made is that such an
//! account on the other side of a resolved token leg is the SOL leg.

use crate::event::{Side, SwapEvent, Venue};
use crate::tx::{FlatIx, RawTransaction, TokenAccountInfo};
use crate::{TOKEN_2022_PROGRAM, TOKEN_PROGRAM, WSOL_MINT};

const SPL_TRANSFER: u8 = 3;
const SPL_TRANSFER_CHECKED: u8 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer<'a> {
    pub source: &'a str,
    pub destination: &'a str,
    pub authority: &'a str,
    /// from `TransferChecked`, else unknown here
    pub mint: Option<&'a str>,
    pub amount: u64,
}

/// SPL / Token-2022 `Transfer` and `TransferChecked` instructions among `ixs`.
pub fn token_transfers<'a>(ixs: &'a [FlatIx<'a>]) -> Vec<Transfer<'a>> {
    let mut out = Vec::new();
    for ix in ixs {
        if ix.program != TOKEN_PROGRAM && ix.program != TOKEN_2022_PROGRAM {
            continue;
        }
        let Some(&op) = ix.data.first() else { continue };
        if ix.data.len() < 9 {
            continue;
        }
        let amount = u64::from_le_bytes(ix.data[1..9].try_into().unwrap());
        match op {
            SPL_TRANSFER if ix.accounts.len() >= 3 => out.push(Transfer {
                source: ix.accounts[0],
                destination: ix.accounts[1],
                authority: ix.accounts[2],
                mint: None,
                amount,
            }),
            SPL_TRANSFER_CHECKED if ix.accounts.len() >= 4 => out.push(Transfer {
                source: ix.accounts[0],
                destination: ix.accounts[2],
                authority: ix.accounts[3],
                mint: Some(ix.accounts[1]),
                amount,
            }),
            _ => {}
        }
    }
    out
}

/// One side of the swap as seen from the wallet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Leg<'a> {
    /// `None` when the account was created and closed within the tx
    mint: Option<&'a str>,
    amount: u64,
}

fn resolve<'a>(
    accounts: &std::collections::HashMap<&'a str, TokenAccountInfo<'a>>,
    account: &str,
    checked_mint: Option<&'a str>,
) -> (Option<&'a str>, Option<&'a str>) {
    let info = accounts.get(account);
    let owner = info.map(|i| i.owner);
    let mint = checked_mint.or(info.map(|i| i.mint));
    (owner, mint)
}

pub fn decode(
    tx: &RawTransaction,
    flat: &[FlatIx<'_>],
    pos: usize,
    venue: Venue,
    tracked: &[String],
) -> Option<SwapEvent> {
    let keys = tx.account_keys();
    let accounts = tx.token_accounts(&keys);
    let transfers = token_transfers(crate::children(flat, pos));
    if transfers.is_empty() {
        return None;
    }

    // Only signers can be the trader; vault authorities are PDAs.
    for wallet in tracked.iter().filter(|w| tx.is_signer(w)) {
        let w = wallet.as_str();
        let mut outgoing: Vec<Leg> = Vec::new();
        let mut incoming: Vec<Leg> = Vec::new();
        let mut unresolved_in: Vec<Leg> = Vec::new();
        for t in &transfers {
            let (src_owner, src_mint) = resolve(&accounts, t.source, t.mint);
            let (dst_owner, dst_mint) = resolve(&accounts, t.destination, t.mint);
            let from_user = t.authority == w || src_owner == Some(w);
            let to_user = dst_owner == Some(w);
            if from_user {
                outgoing.push(Leg { mint: src_mint, amount: t.amount });
            } else if to_user {
                incoming.push(Leg { mint: dst_mint, amount: t.amount });
            } else if dst_owner.is_none() && src_owner.is_some() {
                // vault → account we can't see (temp WSOL closed in-tx)
                unresolved_in.push(Leg { mint: dst_mint, amount: t.amount });
            }
        }
        if outgoing.is_empty() && incoming.is_empty() {
            continue; // this wallet isn't party to the swap
        }
        if incoming.is_empty() && unresolved_in.len() == 1 {
            incoming = unresolved_in;
        }
        if outgoing.len() != 1 || incoming.len() != 1 {
            tracing::debug!(
                sig = tx.signature(), venue = venue.as_str(), out = outgoing.len(), inc = incoming.len(),
                "dex swap legs did not reconcile"
            );
            return None;
        }
        let (out_leg, in_leg) = (&outgoing[0], &incoming[0]);
        let (side, token_mint, token_amount, sol_amount) = match (out_leg.mint, in_leg.mint) {
            (Some(WSOL_MINT), Some(m)) if m != WSOL_MINT => (Side::Buy, m, in_leg.amount, out_leg.amount),
            (Some(m), Some(WSOL_MINT)) if m != WSOL_MINT => (Side::Sell, m, out_leg.amount, in_leg.amount),
            // one leg unresolvable: it is the SOL leg iff the other is a real token
            (Some(m), None) if m != WSOL_MINT => (Side::Sell, m, out_leg.amount, in_leg.amount),
            (None, Some(m)) if m != WSOL_MINT => (Side::Buy, m, in_leg.amount, out_leg.amount),
            _ => {
                tracing::debug!(sig = tx.signature(), venue = venue.as_str(), "dex swap has no SOL<->token shape");
                return None;
            }
        };
        let token_decimals = tx.mint_decimals(token_mint)?;
        return Some(SwapEvent {
            signature: String::new(),
            slot: 0,
            block_time: None,
            wallet: wallet.clone(),
            mint: token_mint.to_string(),
            venue,
            side,
            token_amount,
            token_decimals,
            sol_amount,
            fee_lamports: 0,
            ix_index: flat[pos].ix_index,
        });
    }
    None
}
