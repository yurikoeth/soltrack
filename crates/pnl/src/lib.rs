//! Average-cost-basis PnL per wallet per mint, SOL-denominated, plus the
//! per-wallet statistics a copy-trading screener needs (win rate, hold time,
//! sizing, windowed realized PnL).
//!
//! Pure functions over `SwapEvent`s — no storage, no I/O, no floats.
//! Positions are rebuilt by replaying a wallet's swaps in `(slot, ix_index)`
//! order, or advanced incrementally with [`WalletPnl::apply`].
//!
//! Arithmetic: quantities are `u64` raw token units, lamports are `u128`
//! while held as cost and `i128` once signed. Every product is formed before
//! its division so rounding happens once, at the end.

pub mod tokens;

use std::collections::BTreeMap;

use decode::{Side, SwapEvent};
use serde::{Deserialize, Serialize};

/// Last observed trade price as an exact rational: `lamports / token_units`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub lamports: u64,
    pub token_units: u64,
}

impl Price {
    /// Value of `qty` token units at this price, in lamports (floor).
    pub fn value_of(&self, qty: u64) -> u128 {
        if self.token_units == 0 {
            return 0;
        }
        (qty as u128 * self.lamports as u128) / self.token_units as u128
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub mint: String,
    pub token_decimals: u8,
    /// token units currently held (as seen through decoded swaps)
    pub qty: u64,
    /// total lamports paid for the currently held `qty` (average cost × qty)
    pub cost_lamports: u128,
    /// lamports realized on sells so far, net of venue fees
    pub realized_lamports: i128,
    pub last_price: Option<Price>,
    pub buys: u32,
    pub sells: u32,
    /// Sells that exceeded the tracked quantity (tokens acquired outside the
    /// decoded venues — airdrops, transfers-in, UnknownSwaps). The excess was
    /// treated as zero-basis and counted fully as realized PnL.
    pub oversold_events: u32,
    /// block time of the first buy / first sell we saw, for hold-time stats
    pub first_buy_time: Option<i64>,
    pub first_sell_time: Option<i64>,
    pub last_trade_time: Option<i64>,
}

impl Position {
    fn new(mint: &str, token_decimals: u8) -> Self {
        Self {
            mint: mint.to_string(),
            token_decimals,
            qty: 0,
            cost_lamports: 0,
            realized_lamports: 0,
            last_price: None,
            buys: 0,
            sells: 0,
            oversold_events: 0,
            first_buy_time: None,
            first_sell_time: None,
            last_trade_time: None,
        }
    }

    /// Average cost per token unit as an exact rational, if any is held.
    pub fn avg_cost(&self) -> Option<Price> {
        if self.qty == 0 {
            return None;
        }
        Some(Price {
            lamports: u64::try_from(self.cost_lamports).unwrap_or(u64::MAX),
            token_units: self.qty,
        })
    }

    /// Mark-to-last-trade value of the held quantity, in lamports.
    pub fn value_lamports(&self) -> u128 {
        match self.last_price {
            Some(p) if self.qty > 0 => p.value_of(self.qty),
            _ => 0,
        }
    }

    /// Mark-to-last-trade value minus cost, in lamports.
    pub fn unrealized_lamports(&self) -> i128 {
        match self.last_price {
            Some(p) if self.qty > 0 => p.value_of(self.qty) as i128 - self.cost_lamports as i128,
            _ => 0,
        }
    }

    /// Time from first buy to first sell, if both happened.
    pub fn hold_secs(&self) -> Option<i64> {
        match (self.first_buy_time, self.first_sell_time) {
            (Some(b), Some(s)) => Some((s - b).max(0)),
            _ => None,
        }
    }

    /// Apply one swap; returns the realized PnL delta (0 for buys).
    pub fn apply(&mut self, ev: &SwapEvent) -> i128 {
        self.token_decimals = ev.token_decimals;
        if ev.token_amount > 0 {
            self.last_price = Some(Price {
                lamports: ev.sol_amount,
                token_units: ev.token_amount,
            });
        }
        if ev.block_time.is_some() {
            self.last_trade_time = ev.block_time;
        }
        match ev.side {
            Side::Buy => {
                self.buys += 1;
                if self.first_buy_time.is_none() {
                    self.first_buy_time = ev.block_time;
                }
                self.qty = self.qty.saturating_add(ev.token_amount);
                self.cost_lamports += ev.sol_amount as u128;
                0
            }
            Side::Sell => {
                self.sells += 1;
                if self.first_sell_time.is_none() {
                    self.first_sell_time = ev.block_time;
                }
                let sold = ev.token_amount.min(self.qty);
                if ev.token_amount > self.qty {
                    self.oversold_events += 1;
                    tracing::warn!(
                        sig = %ev.signature, mint = %ev.mint, wallet = %ev.wallet,
                        held = self.qty, sold = ev.token_amount,
                        "sell exceeds tracked position; excess treated as zero-basis"
                    );
                }
                // proportional share of the cost basis leaves with the tokens
                let cost_removed = if self.qty == 0 {
                    0
                } else {
                    (self.cost_lamports * sold as u128) / self.qty as u128
                };
                let delta = ev.sol_amount as i128 - cost_removed as i128;
                self.realized_lamports += delta;
                self.qty -= sold;
                self.cost_lamports -= cost_removed;
                if self.qty == 0 {
                    // don't let floor-division dust survive a full exit
                    self.cost_lamports = 0;
                }
                delta
            }
        }
    }
}

/// One sell, kept so realized PnL can be windowed by time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SellRecord {
    pub mint: String,
    pub block_time: Option<i64>,
    pub sol_amount: u64,
    pub realized_delta: i128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletPnl {
    pub wallet: String,
    pub positions: BTreeMap<String, Position>,
    pub last_trade: Option<SwapEvent>,
    pub trade_count: u32,
    pub sells: Vec<SellRecord>,
    /// SOL paid on every buy, in order — for sizing stats.
    pub buy_sizes: Vec<u64>,
}

/// Screener statistics for one wallet. Windowed values use block time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletStats {
    pub trades: u32,
    pub buys: u32,
    pub sells: u32,
    pub tokens_traded: u32,
    /// tokens with at least one sell
    pub tokens_with_exits: u32,
    /// tokens with exits whose realized PnL is > 0 / < 0 (break-even counts as neither)
    pub wins: u32,
    pub losses: u32,
    /// wins / (wins + losses), in basis points
    pub win_rate_bp: Option<u32>,
    /// median of (first sell − first buy) over tokens with both
    pub median_hold_secs: Option<i64>,
    pub median_buy_lamports: Option<u64>,
    pub max_buy_lamports: u64,
    pub bought_lamports: u128,
    pub sold_lamports: u128,
    pub realized_24h: i128,
    pub realized_7d: i128,
    pub realized_30d: i128,
    pub oversold_events: u32,
}

impl WalletPnl {
    pub fn new(wallet: &str) -> Self {
        Self {
            wallet: wallet.to_string(),
            positions: BTreeMap::new(),
            last_trade: None,
            trade_count: 0,
            sells: Vec::new(),
            buy_sizes: Vec::new(),
        }
    }

    /// Rebuild from a wallet's full history. `swaps` must be in
    /// `(slot, ix_index)` order; only events for `wallet` are applied.
    pub fn compute(wallet: &str, swaps: &[SwapEvent]) -> Self {
        let mut w = Self::new(wallet);
        for ev in swaps {
            w.apply(ev);
        }
        w
    }

    pub fn apply(&mut self, ev: &SwapEvent) {
        if ev.wallet != self.wallet {
            return;
        }
        let delta = self
            .positions
            .entry(ev.mint.clone())
            .or_insert_with(|| Position::new(&ev.mint, ev.token_decimals))
            .apply(ev);
        match ev.side {
            Side::Buy => self.buy_sizes.push(ev.sol_amount),
            Side::Sell => self.sells.push(SellRecord {
                mint: ev.mint.clone(),
                block_time: ev.block_time,
                sol_amount: ev.sol_amount,
                realized_delta: delta,
            }),
        }
        self.trade_count += 1;
        let newer = match &self.last_trade {
            None => true,
            Some(prev) => (ev.slot, ev.ix_index) >= (prev.slot, prev.ix_index),
        };
        if newer {
            self.last_trade = Some(ev.clone());
        }
    }

    pub fn realized_lamports(&self) -> i128 {
        self.positions.values().map(|p| p.realized_lamports).sum()
    }

    pub fn unrealized_lamports(&self) -> i128 {
        self.positions.values().map(Position::unrealized_lamports).sum()
    }

    /// Positions with tokens still held.
    pub fn open_positions(&self) -> impl Iterator<Item = &Position> {
        self.positions.values().filter(|p| p.qty > 0)
    }

    pub fn median_buy_lamports(&self) -> Option<u64> {
        median_u64(&self.buy_sizes)
    }

    /// Realized PnL from sells at or after `cutoff` (unix secs). Sells with
    /// no block time are excluded.
    pub fn realized_since(&self, cutoff: i64) -> i128 {
        self.sells
            .iter()
            .filter(|s| s.block_time.map_or(false, |t| t >= cutoff))
            .map(|s| s.realized_delta)
            .sum()
    }

    pub fn stats(&self, now: i64) -> WalletStats {
        let mut wins = 0;
        let mut losses = 0;
        let mut tokens_with_exits = 0;
        let mut holds: Vec<i64> = Vec::new();
        for p in self.positions.values() {
            if p.sells > 0 {
                tokens_with_exits += 1;
                if p.realized_lamports > 0 {
                    wins += 1;
                } else if p.realized_lamports < 0 {
                    losses += 1;
                }
            }
            if let Some(h) = p.hold_secs() {
                holds.push(h);
            }
        }
        let decided = wins + losses;
        WalletStats {
            trades: self.trade_count,
            buys: self.buy_sizes.len() as u32,
            sells: self.sells.len() as u32,
            tokens_traded: self.positions.len() as u32,
            tokens_with_exits,
            wins,
            losses,
            win_rate_bp: (decided > 0).then(|| wins * 10_000 / decided),
            median_hold_secs: median_i64(&holds),
            median_buy_lamports: self.median_buy_lamports(),
            max_buy_lamports: self.buy_sizes.iter().copied().max().unwrap_or(0),
            bought_lamports: self.buy_sizes.iter().map(|&b| b as u128).sum(),
            sold_lamports: self.sells.iter().map(|s| s.sol_amount as u128).sum(),
            realized_24h: self.realized_since(now - 86_400),
            realized_7d: self.realized_since(now - 7 * 86_400),
            realized_30d: self.realized_since(now - 30 * 86_400),
            oversold_events: self.positions.values().map(|p| p.oversold_events).sum(),
        }
    }
}

/// Lower median (no averaging, stays in the value's domain).
fn median_u64(v: &[u64]) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    Some(s[(s.len() - 1) / 2])
}

fn median_i64(v: &[i64]) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    Some(s[(s.len() - 1) / 2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use decode::Venue;

    const W: &str = "Wallet111111111111111111111111111111111111111";
    const M: &str = "Mint1111111111111111111111111111111111111111";
    const SOL: u64 = 1_000_000_000;

    fn ev(slot: u64, side: Side, tokens: u64, lamports: u64) -> SwapEvent {
        SwapEvent {
            signature: format!("sig{slot}"),
            slot,
            block_time: None,
            wallet: W.into(),
            mint: M.into(),
            venue: Venue::PumpFunCurve,
            side,
            token_amount: tokens,
            token_decimals: 6,
            sol_amount: lamports,
            fee_lamports: 0,
            ix_index: 0,
        }
    }

    fn ev_at(slot: u64, t: i64, mint: &str, side: Side, tokens: u64, lamports: u64) -> SwapEvent {
        let mut e = ev(slot, side, tokens, lamports);
        e.block_time = Some(t);
        e.mint = mint.into();
        e
    }

    /// The hand-verifiable case from the spec: buy, buy, sell.
    ///
    ///   buy  1000 tokens for 1.0 SOL   → avg 0.001 SOL/token
    ///   buy  1000 tokens for 3.0 SOL   → 2000 held, cost 4.0, avg 0.002
    ///   sell  500 tokens for 2.0 SOL   → basis removed 1.0, realized +1.0
    ///                                    1500 held, cost 3.0, last px 0.004
    ///                                    unrealized = 1500×0.004 − 3.0 = +3.0
    #[test]
    fn buy_buy_sell_hand_verified() {
        let swaps = vec![
            ev(1, Side::Buy, 1000, SOL),
            ev(2, Side::Buy, 1000, 3 * SOL),
            ev(3, Side::Sell, 500, 2 * SOL),
        ];
        let w = WalletPnl::compute(W, &swaps);
        let p = &w.positions[M];
        assert_eq!(p.qty, 1500);
        assert_eq!(p.cost_lamports, 3 * SOL as u128);
        assert_eq!(p.realized_lamports, SOL as i128);
        assert_eq!(p.unrealized_lamports(), 3 * SOL as i128);
        assert_eq!(w.realized_lamports(), SOL as i128);
        assert_eq!(w.unrealized_lamports(), 3 * SOL as i128);
        assert_eq!(w.trade_count, 3);
        assert_eq!(w.last_trade.as_ref().unwrap().slot, 3);
        assert_eq!(p.avg_cost(), Some(Price { lamports: 3 * SOL, token_units: 1500 }));
        assert_eq!(w.sells.len(), 1);
        assert_eq!(w.sells[0].realized_delta, SOL as i128);
        assert_eq!(w.buy_sizes, vec![SOL, 3 * SOL]);
    }

    #[test]
    fn full_exit_clears_basis_and_realizes_everything() {
        let swaps = vec![
            ev(1, Side::Buy, 3, 10), // 3 tokens for 10 lamports: avg 3.33…
            ev(2, Side::Sell, 1, 5), // removes floor(10×1/3)=3, realized +2
            ev(3, Side::Sell, 2, 5), // removes remaining 7, realized −2
        ];
        let w = WalletPnl::compute(W, &swaps);
        let p = &w.positions[M];
        assert_eq!(p.qty, 0);
        assert_eq!(p.cost_lamports, 0);
        assert_eq!(p.realized_lamports, 0); // paid 10, got back 10
        assert_eq!(p.unrealized_lamports(), 0);
        assert!(w.open_positions().next().is_none());
    }

    #[test]
    fn incremental_apply_matches_full_replay() {
        let swaps = vec![
            ev(1, Side::Buy, 1_000_000_000, 250_000_000),
            ev(2, Side::Buy, 400_000_000, 300_000_000),
            ev(3, Side::Sell, 700_000_000, 500_000_000),
            ev(4, Side::Buy, 100_000_000, 90_000_000),
            ev(5, Side::Sell, 800_000_000, 1_200_000_000),
        ];
        let full = WalletPnl::compute(W, &swaps);
        let mut inc = WalletPnl::new(W);
        for s in &swaps {
            inc.apply(s);
        }
        assert_eq!(full, inc);
        assert_eq!(full.positions[M].qty, 0);
        assert_eq!(full.positions[M].realized_lamports, 1_700_000_000 - 640_000_000);
    }

    #[test]
    fn oversell_is_zero_basis_and_flagged() {
        let swaps = vec![
            ev(1, Side::Buy, 100, 100),
            ev(2, Side::Sell, 150, 300), // 50 more than held
        ];
        let w = WalletPnl::compute(W, &swaps);
        let p = &w.positions[M];
        assert_eq!(p.qty, 0);
        assert_eq!(p.cost_lamports, 0);
        assert_eq!(p.realized_lamports, 200);
        assert_eq!(p.oversold_events, 1);
        assert_eq!(w.stats(0).oversold_events, 1);
    }

    #[test]
    fn sell_with_nothing_held_is_pure_realized() {
        let w = WalletPnl::compute(W, &[ev(1, Side::Sell, 10, 77)]);
        let p = &w.positions[M];
        assert_eq!(p.realized_lamports, 77);
        assert_eq!(p.qty, 0);
        assert_eq!(p.unrealized_lamports(), 0);
    }

    #[test]
    fn other_wallets_are_ignored_and_mints_kept_separate() {
        let mut other = ev(1, Side::Buy, 10, 10);
        other.wallet = "Other".into();
        let mut m2 = ev(2, Side::Buy, 5, 50);
        m2.mint = "Mint2".into();
        let w = WalletPnl::compute(W, &[other, ev(1, Side::Buy, 10, 10), m2]);
        assert_eq!(w.trade_count, 2);
        assert_eq!(w.positions.len(), 2);
        assert_eq!(w.positions[M].qty, 10);
        assert_eq!(w.positions["Mint2"].cost_lamports, 50);
    }

    #[test]
    fn big_numbers_do_not_overflow() {
        // 1e15 raw units (a whole 1B-supply 6-decimal token) at 500 SOL
        let big = ev(1, Side::Buy, 1_000_000_000_000_000, 500 * SOL);
        let mut sell = ev(2, Side::Sell, 1_000_000_000_000_000, 5_000 * SOL);
        sell.slot = 2;
        let w = WalletPnl::compute(W, &[big, sell]);
        assert_eq!(w.positions[M].realized_lamports, 4_500 * SOL as i128);
    }

    /// Three tokens: A won (+1 SOL, held 10 min), B lost (−0.5 SOL, held 1 h,
    /// 8 days ago), C still open. Plus one break-even token D that counts
    /// as neither win nor loss.
    #[test]
    fn stats_win_rate_hold_time_sizing_and_windows() {
        let now = 1_800_000_000;
        let day = 86_400;
        let swaps = vec![
            // B: 8 days ago, loss
            ev_at(1, now - 8 * day, "B", Side::Buy, 100, 2 * SOL),
            ev_at(2, now - 8 * day + 3600, "B", Side::Sell, 100, SOL + SOL / 2),
            // D: 3 days ago, break-even
            ev_at(3, now - 3 * day, "D", Side::Buy, 10, SOL / 2),
            ev_at(4, now - 3 * day + 60, "D", Side::Sell, 10, SOL / 2),
            // A: 2 hours ago, win
            ev_at(5, now - 7200, "A", Side::Buy, 1000, SOL),
            ev_at(6, now - 7200 + 600, "A", Side::Sell, 1000, 2 * SOL),
            // C: open
            ev_at(7, now - 60, "C", Side::Buy, 5, 4 * SOL),
        ];
        let w = WalletPnl::compute(W, &swaps);
        let s = w.stats(now);
        assert_eq!(s.trades, 7);
        assert_eq!((s.buys, s.sells), (4, 3));
        assert_eq!(s.tokens_traded, 4);
        assert_eq!(s.tokens_with_exits, 3);
        assert_eq!((s.wins, s.losses), (1, 1));
        assert_eq!(s.win_rate_bp, Some(5_000));
        // holds: B 3600, D 60, A 600 → sorted 60, 600, 3600 → median 600
        assert_eq!(s.median_hold_secs, Some(600));
        // buys: 2, 0.5, 1, 4 SOL → sorted 0.5, 1, 2, 4 → lower median 1
        assert_eq!(s.median_buy_lamports, Some(SOL));
        assert_eq!(s.max_buy_lamports, 4 * SOL);
        assert_eq!(s.bought_lamports, (2 * SOL + SOL / 2 + SOL + 4 * SOL) as u128);
        assert_eq!(s.sold_lamports, (SOL + SOL / 2 + SOL / 2 + 2 * SOL) as u128);
        // windows: 24h has only A (+1); 7d has A + D (+1 + 0); 30d adds B (−0.5)
        assert_eq!(s.realized_24h, SOL as i128);
        assert_eq!(s.realized_7d, SOL as i128);
        assert_eq!(s.realized_30d, SOL as i128 - (SOL / 2) as i128);
        assert_eq!(w.realized_lamports(), s.realized_30d);
    }

    #[test]
    fn stats_on_empty_wallet_are_all_none_or_zero() {
        let s = WalletPnl::new(W).stats(0);
        assert_eq!(s.win_rate_bp, None);
        assert_eq!(s.median_hold_secs, None);
        assert_eq!(s.median_buy_lamports, None);
        assert_eq!(s.realized_30d, 0);
    }
}
