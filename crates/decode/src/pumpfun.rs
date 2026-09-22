//! Pump.fun bonding curve. Every trade — legacy `buy`/`sell`, the newer
//! `buy_v2`/`sell_v2`, and whatever comes next — emits a `TradeEvent` via
//! `emit_cpi!`. We decode that event rather than the instruction args, which
//! only carry slippage limits.
//!
//! `TradeEvent` layout (borsh, after the 8-byte event discriminator):
//! ```text
//!   0  mint                     [u8;32]
//!  32  sol_amount               u64   bonding-curve leg, EXCLUDING fees
//!  40  token_amount             u64
//!  48  is_buy                   bool
//!  49  user                     [u8;32]
//!  81  timestamp                i64
//!  89  virtual_sol_reserves     u64
//!  97  virtual_token_reserves   u64
//! 105  real_sol_reserves        u64
//! 113  real_token_reserves      u64
//! --- fields below exist on events >= 217 bytes (2025+) ---
//! 121  fee_recipient            [u8;32]
//! 153  fee_basis_points         u64
//! 161  fee                      u64   protocol fee, lamports
//! 169  creator                  [u8;32]
//! 201  creator_fee_basis_points u64
//! 209  creator_fee              u64   lamports
//! 217  ... (volume accumulators, ix name string, etc. — ignored)
//! ```
//! Verified against fixtures: `sol_amount` equals the bonding curve's lamport
//! delta; `fee` and `creator_fee` equal the fee-recipient / creator-vault
//! deltas; the trader's net = sol_amount ± (fee + creator_fee).

use crate::anchor::{discriminator, Reader};
use crate::event::{Side, SwapEvent, Venue};
use crate::tx::{FlatIx, RawTransaction};

pub const PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// `CreateEvent`, emitted once when a token is launched:
/// `name, symbol, uri: String; mint, bonding_curve, user: Pubkey;
///  [creator: Pubkey]; timestamp: i64; virtual_token_reserves,
///  virtual_sol_reserves, real_token_reserves, token_total_supply: u64`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEvent {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub mint: String,
    pub bonding_curve: String,
    pub user: String,
    pub creator: Option<String>,
    pub timestamp: i64,
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub token_total_supply: u64,
}

impl CreateEvent {
    /// Launch market cap: price × supply, lamports.
    pub fn mcap_lamports(&self) -> u128 {
        if self.virtual_token_reserves == 0 {
            return 0;
        }
        self.virtual_sol_reserves as u128 * self.token_total_supply as u128 / self.virtual_token_reserves as u128
    }
}

pub fn parse_create_event(payload: &[u8]) -> Option<CreateEvent> {
    if payload.len() < 8 || payload[..8] != discriminator("event", "CreateEvent") {
        return None;
    }
    let mut r = Reader::new(&payload[8..]);
    let name = r.string()?;
    let symbol = r.string()?;
    let uri = r.string()?;
    let mint = r.pubkey()?;
    let bonding_curve = r.pubkey()?;
    let user = r.pubkey()?;
    // newer events carry `creator` before the timestamp
    let creator = if r.remaining() >= 32 + 8 + 8 * 4 { r.pubkey() } else { None };
    let timestamp = r.i64()?;
    let virtual_token_reserves = r.u64()?;
    let virtual_sol_reserves = r.u64()?;
    let real_token_reserves = r.u64()?;
    let token_total_supply = r.u64()?;
    Some(CreateEvent {
        name: name.trim().to_string(),
        symbol: symbol.trim().to_string(),
        uri: uri.trim().to_string(),
        mint,
        bonding_curve,
        user,
        creator,
        timestamp,
        virtual_token_reserves,
        virtual_sol_reserves,
        real_token_reserves,
        token_total_supply,
    })
}

/// A launch as seen in one transaction: the `CreateEvent` plus the dev's
/// buy in the same transaction, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub create: CreateEvent,
    pub dev_buy_lamports: u64,
    pub dev_buy_tokens: u64,
}

/// Find a Pump.fun launch in a transaction (any Pump.fun instruction that
/// emitted a `CreateEvent`).
pub fn find_launch(tx: &RawTransaction) -> Option<Launch> {
    let keys = tx.account_keys();
    let flat = tx.flatten_instructions(&keys);
    let mut create: Option<CreateEvent> = None;
    let mut trade: Option<TradeEvent> = None;
    for ix in &flat {
        if ix.program != PROGRAM_ID || !ix.data.starts_with(&crate::anchor::EVENT_IX_PREFIX) {
            continue;
        }
        let payload = &ix.data[8..];
        if create.is_none() {
            create = parse_create_event(payload);
        }
        if trade.is_none() {
            trade = parse_trade_event(payload);
        }
    }
    let create = create?;
    let (dev_buy_lamports, dev_buy_tokens) = match trade {
        Some(t) if t.is_buy && t.mint == create.mint && t.user == create.user => {
            let fees = t.fee.unwrap_or(0).saturating_add(t.creator_fee.unwrap_or(0));
            (t.sol_amount.saturating_add(fees), t.token_amount)
        }
        _ => (0, 0),
    };
    Some(Launch {
        create,
        dev_buy_lamports,
        dev_buy_tokens,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeEvent {
    pub mint: String,
    pub sol_amount: u64,
    pub token_amount: u64,
    pub is_buy: bool,
    pub user: String,
    pub timestamp: i64,
    /// `None` on legacy events that predate on-chain fee reporting.
    pub fee: Option<u64>,
    pub creator_fee: Option<u64>,
}

pub fn parse_trade_event(payload: &[u8]) -> Option<TradeEvent> {
    let disc = discriminator("event", "TradeEvent");
    if payload.len() < 8 || payload[..8] != disc {
        return None;
    }
    let mut r = Reader::new(&payload[8..]);
    let mint = r.pubkey()?;
    let sol_amount = r.u64()?;
    let token_amount = r.u64()?;
    let is_buy = r.bool()?;
    let user = r.pubkey()?;
    let timestamp = r.i64()?;
    r.skip(8 * 4)?; // reserves
    let (fee, creator_fee) = if r.remaining() >= 96 {
        r.skip(32)?; // fee_recipient
        r.skip(8)?; // fee_basis_points
        let fee = r.u64()?;
        r.skip(32)?; // creator
        r.skip(8)?; // creator_fee_basis_points
        let creator_fee = r.u64()?;
        (Some(fee), Some(creator_fee))
    } else {
        (None, None)
    };
    Some(TradeEvent {
        mint,
        sol_amount,
        token_amount,
        is_buy,
        user,
        timestamp,
        fee,
        creator_fee,
    })
}

/// Decode a Pump.fun instruction given the event it emitted.
/// Returns a `SwapEvent` with tx-level fields (signature, slot, block_time)
/// left for the caller to fill.
pub fn decode(tx: &RawTransaction, ix: &FlatIx<'_>, event_payload: &[u8]) -> Option<SwapEvent> {
    let ev = parse_trade_event(event_payload)?;
    let Some(token_decimals) = tx.mint_decimals(&ev.mint) else {
        tracing::warn!(sig = tx.signature(), mint = %ev.mint, "pump.fun trade without token balance metadata; skipping");
        return None;
    };
    if ev.fee.is_none() {
        tracing::warn!(sig = tx.signature(), "legacy TradeEvent without fee fields; treating fees as 0");
    }
    let fee_lamports = ev.fee.unwrap_or(0).saturating_add(ev.creator_fee.unwrap_or(0));
    let (side, sol_amount) = if ev.is_buy {
        (Side::Buy, ev.sol_amount.checked_add(fee_lamports)?)
    } else {
        (Side::Sell, ev.sol_amount.checked_sub(fee_lamports)?)
    };
    Some(SwapEvent {
        signature: String::new(),
        slot: 0,
        block_time: None,
        wallet: ev.user,
        mint: ev.mint,
        venue: Venue::PumpFunCurve,
        side,
        token_amount: ev.token_amount,
        token_decimals,
        sol_amount,
        fee_lamports,
        ix_index: ix.ix_index,
    })
}
