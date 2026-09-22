//! PumpSwap AMM. `buy`, `buy_exact_quote_in` and `sell` all emit a
//! `BuyEvent` / `SellEvent` via `emit_cpi!`; we decode those.
//!
//! The events do not carry mints, so `base_mint` / `quote_mint` come from the
//! parent instruction's accounts (indices 3 and 4 in every observed layout).
//! Pools created by Pump.fun migration have `base = token, quote = WSOL`, but
//! inverted pools (`base = WSOL`) exist and are handled — there a `SellEvent`
//! is the wallet *buying* the token. Pools with no WSOL leg are not decoded.
//!
//! Shared prefix of both events (borsh, after the 8-byte discriminator):
//! ```text
//!   0  timestamp                    i64
//!   8  base_amount_out / _in        u64
//!  16  max_quote_in / min_quote_out u64
//!  24  user_base_token_reserves     u64
//!  32  user_quote_token_reserves    u64
//!  40  pool_base_token_reserves     u64
//!  48  pool_quote_token_reserves    u64
//!  56  quote_amount_in / _out       u64
//!  64  lp_fee_basis_points          u64
//!  72  lp_fee                       u64
//!  80  protocol_fee_basis_points    u64
//!  88  protocol_fee                 u64
//!  96  quote_amount_in_with_lp_fee / quote_amount_out_without_lp_fee  u64
//! 104  user_quote_amount_in / _out  u64
//! 112  pool, user, user_base_ata, user_quote_ata,
//!      protocol_fee_recipient, protocol_fee_recipient_ata, coin_creator  7 x [u8;32]
//! 336  coin_creator_fee_basis_points u64
//! 344  coin_creator_fee             u64
//! ```
//! Verified against fixtures: on a buy the wallet pays
//! `quote_amount_in_with_lp_fee + protocol_fee + coin_creator_fee` (the
//! `user_quote_amount_in` field means different things in `buy` vs
//! `buy_exact_quote_in`, so it is not used); on a sell the wallet receives
//! exactly `user_quote_amount_out`.

use crate::anchor::{discriminator, Reader};
use crate::event::{Side, SwapEvent, Venue};
use crate::tx::{FlatIx, RawTransaction};
use crate::WSOL_MINT;

pub const PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// wallet gives quote, receives base
    BuyBase,
    /// wallet gives base, receives quote
    SellBase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmmEvent {
    pub direction: Direction,
    pub user: String,
    /// base tokens received (BuyBase) or given (SellBase)
    pub base_amount: u64,
    /// quote units the wallet actually paid (BuyBase) or received (SellBase),
    /// inclusive of / net of all fees
    pub quote_amount: u64,
    /// lp + protocol + creator fee, in quote units
    pub total_fee_quote: u64,
}

pub fn parse_amm_event(payload: &[u8]) -> Option<AmmEvent> {
    if payload.len() < 8 {
        return None;
    }
    let direction = if payload[..8] == discriminator("event", "BuyEvent") {
        Direction::BuyBase
    } else if payload[..8] == discriminator("event", "SellEvent") {
        Direction::SellBase
    } else {
        return None;
    };
    let mut r = Reader::new(&payload[8..]);
    r.i64()?; // timestamp
    let base_amount = r.u64()?;
    r.u64()?; // max_quote_in / min_quote_out
    r.skip(8 * 4)?; // reserves
    r.u64()?; // quote_amount_in / quote_amount_out
    r.u64()?; // lp_fee_basis_points
    let lp_fee = r.u64()?;
    r.u64()?; // protocol_fee_basis_points
    let protocol_fee = r.u64()?;
    let quote_with_lp_fee = r.u64()?; // buy: in_with_lp_fee, sell: out_without_lp_fee
    let user_quote = r.u64()?;
    r.skip(32)?; // pool
    let user = r.pubkey()?;
    r.skip(32 * 5)?; // token accounts, fee recipient(s), coin_creator
    r.u64()?; // coin_creator_fee_basis_points
    let coin_creator_fee = r.u64()?;

    let total_fee_quote = lp_fee
        .checked_add(protocol_fee)?
        .checked_add(coin_creator_fee)?;
    let quote_amount = match direction {
        Direction::BuyBase => quote_with_lp_fee
            .checked_add(protocol_fee)?
            .checked_add(coin_creator_fee)?,
        Direction::SellBase => user_quote,
    };
    Some(AmmEvent {
        direction,
        user,
        base_amount,
        quote_amount,
        total_fee_quote,
    })
}

pub fn decode(tx: &RawTransaction, ix: &FlatIx<'_>, event_payload: &[u8]) -> Option<SwapEvent> {
    let ev = parse_amm_event(event_payload)?;
    let (&base_mint, &quote_mint) = (ix.accounts.get(3)?, ix.accounts.get(4)?);

    let (mint, side, token_amount, sol_amount, fee_lamports) =
        match (base_mint == WSOL_MINT, quote_mint == WSOL_MINT) {
            // normal pool: token is base, SOL is quote
            (false, true) => {
                let side = match ev.direction {
                    Direction::BuyBase => Side::Buy,
                    Direction::SellBase => Side::Sell,
                };
                (base_mint, side, ev.base_amount, ev.quote_amount, ev.total_fee_quote)
            }
            // inverted pool: SOL is base, token is quote; fees are taken in tokens
            (true, false) => {
                let side = match ev.direction {
                    Direction::BuyBase => Side::Sell,
                    Direction::SellBase => Side::Buy,
                };
                (quote_mint, side, ev.quote_amount, ev.base_amount, 0)
            }
            _ => {
                tracing::debug!(sig = tx.signature(), base = base_mint, quote = quote_mint, "pumpswap pool without a WSOL leg; skipping");
                return None;
            }
        };
    let Some(token_decimals) = tx.mint_decimals(mint) else {
        tracing::warn!(sig = tx.signature(), mint, "pumpswap trade without token balance metadata; skipping");
        return None;
    };
    Some(SwapEvent {
        signature: String::new(),
        slot: 0,
        block_time: None,
        wallet: ev.user,
        mint: mint.to_string(),
        venue: Venue::PumpSwapAmm,
        side,
        token_amount,
        token_decimals,
        sol_amount,
        fee_lamports,
        ix_index: ix.ix_index,
    })
}
