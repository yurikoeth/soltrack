//! Jupiter v6 aggregator. Every route leg emits a `SwapEvent` via
//! `emit_cpi!`; netting the legs per mint gives exactly what the user put in
//! and took out, regardless of which pools were used.
//!
//! `SwapEvent` (after the 8-byte event discriminator, 112 bytes):
//! ```text
//!   0  amm            [u8;32]
//!  32  input_mint     [u8;32]
//!  64  input_amount   u64
//!  72  output_mint    [u8;32]
//! 104  output_amount  u64
//! ```
//! Jupiter's platform fee, if any, is taken from the output leg *before*
//! the last SwapEvent... no — it is a separate `FeeEvent`/transfer after the
//! route; the netted amounts are what left/arrived in the pools. The wallet's
//! actual token delta is used to attribute the trade, and must match the
//! netted direction.

use std::collections::BTreeMap;

use crate::anchor::{discriminator, Reader};
use crate::event::{Side, SwapEvent, Venue};
use crate::tx::{FlatIx, RawTransaction};
use crate::WSOL_MINT;

pub const PROGRAM_ID: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leg {
    pub amm: String,
    pub input_mint: String,
    pub input_amount: u64,
    pub output_mint: String,
    pub output_amount: u64,
}

pub fn parse_swap_event(payload: &[u8]) -> Option<Leg> {
    if payload.len() < 8 || payload[..8] != discriminator("event", "SwapEvent") {
        return None;
    }
    let mut r = Reader::new(&payload[8..]);
    Some(Leg {
        amm: r.pubkey()?,
        input_mint: r.pubkey()?,
        input_amount: r.u64()?,
        output_mint: r.pubkey()?,
        output_amount: r.u64()?,
    })
}

/// Net flow per mint across legs: negative = spent, positive = received.
/// Intermediate hops cancel out.
pub fn net_flows(legs: &[Leg]) -> BTreeMap<String, i128> {
    let mut net: BTreeMap<String, i128> = BTreeMap::new();
    for l in legs {
        *net.entry(l.input_mint.clone()).or_default() -= l.input_amount as i128;
        *net.entry(l.output_mint.clone()).or_default() += l.output_amount as i128;
    }
    net.retain(|_, v| *v != 0);
    net
}

pub fn decode(tx: &RawTransaction, flat: &[FlatIx<'_>], pos: usize, tracked: &[String]) -> Option<SwapEvent> {
    let legs: Vec<Leg> = crate::find_events(flat, pos, PROGRAM_ID)
        .into_iter()
        .filter_map(parse_swap_event)
        .collect();
    if legs.is_empty() {
        return None;
    }
    let net = net_flows(&legs);
    if net.len() != 2 || !net.contains_key(WSOL_MINT) {
        // SOL→SOL arbitrage, token→token, or a multi-token route: not a
        // token-for-SOL trade we can book.
        tracing::debug!(sig = tx.signature(), mints = net.len(), "jupiter route is not a SOL<->token swap");
        return None;
    }
    let sol_net = net[WSOL_MINT];
    let (mint, token_net) = net.iter().find(|(m, _)| m.as_str() != WSOL_MINT)?;
    let (side, token_amount, sol_amount) = match (sol_net < 0, *token_net > 0) {
        (true, true) => (Side::Buy, u64::try_from(*token_net).ok()?, u64::try_from(-sol_net).ok()?),
        (false, false) => (Side::Sell, u64::try_from(-*token_net).ok()?, u64::try_from(sol_net).ok()?),
        _ => return None,
    };

    // The router's `user` is not in the event. The user is the signer who
    // authorised the route's token transfers; pool PDAs never sign.
    let transfers = crate::dex::token_transfers(crate::children(flat, pos));
    let wallet = tracked
        .iter()
        .find(|w| tx.is_signer(w) && transfers.iter().any(|t| t.authority == w.as_str()))?;
    let token_decimals = tx.mint_decimals(mint)?;
    Some(SwapEvent {
        signature: String::new(),
        slot: 0,
        block_time: None,
        wallet: wallet.clone(),
        mint: mint.clone(),
        venue: Venue::Jupiter,
        side,
        token_amount,
        token_decimals,
        sol_amount,
        fee_lamports: 0,
        ix_index: flat[pos].ix_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(i: &str, ia: u64, o: &str, oa: u64) -> Leg {
        Leg {
            amm: "amm".into(),
            input_mint: i.into(),
            input_amount: ia,
            output_mint: o.into(),
            output_amount: oa,
        }
    }

    #[test]
    fn two_hop_route_nets_out_the_intermediate() {
        let legs = [leg(WSOL_MINT, 1_000, "USDC", 150), leg("USDC", 150, "MEME", 999)];
        let net = net_flows(&legs);
        assert_eq!(net.len(), 2);
        assert_eq!(net[WSOL_MINT], -1_000);
        assert_eq!(net["MEME"], 999);
    }

    #[test]
    fn split_route_sums_legs() {
        let legs = [leg("MEME", 400, WSOL_MINT, 10), leg("MEME", 600, WSOL_MINT, 14)];
        let net = net_flows(&legs);
        assert_eq!(net["MEME"], -1_000);
        assert_eq!(net[WSOL_MINT], 24);
    }

    #[test]
    fn arbitrage_leaves_only_sol() {
        let legs = [leg(WSOL_MINT, 100, "USDC", 5), leg("USDC", 5, "USDT", 5), leg("USDT", 5, WSOL_MINT, 101)];
        let net = net_flows(&legs);
        assert_eq!(net.len(), 1);
        assert_eq!(net[WSOL_MINT], 1);
    }
}
