//! Paper-trading simulation: replay a wallet's decoded swaps and book what a
//! copier would have done — buy after a delay at a worse price, mirror the
//! target's sells proportionally (also delayed), optional stop / take-profit.
//!
//! Prices are only observable at the target's own trades (there is no
//! market feed here), so:
//! * the entry price is the target's price worsened by `slippage_bp`;
//! * the delay is modelled structurally: our buy *fills* `delay_secs` after
//!   the target's, and a target sell that lands before our fill cannot be
//!   mirrored — we end up holding (counted in `missed_exits`);
//! * `adverse_entries` counts fills where the next observed price in that
//!   mint was already below our entry — a proxy for "we bought the top".
//!
//! Integer math throughout; products before divisions.

use std::collections::BTreeMap;

use decode::{Side, SwapEvent};
use serde::{Deserialize, Serialize};

const BP: u128 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Sizing {
    /// Same SOL amount on every copied buy.
    Fixed { lamports: u64 },
    /// A fraction (basis points) of the target's buy, capped.
    Fraction { bp: u32, cap_lamports: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimConfig {
    /// Seconds between the target's trade and our fill.
    pub delay_secs: u32,
    /// Price worsening applied to both our buys and sells, basis points.
    pub slippage_bp: u32,
    pub sizing: Sizing,
    /// Mirror the target's sells (proportionally to what they sold).
    pub mirror_sells: bool,
    /// Exit fully when marked value is this far below cost (bp).
    pub stop_loss_bp: Option<u32>,
    /// Exit fully when marked value is this far above cost (bp).
    pub take_profit_bp: Option<u32>,
    /// Ignore target buys smaller than this (noise / dust).
    pub min_target_buy_lamports: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            delay_secs: 15,
            slippage_bp: 200,
            sizing: Sizing::Fixed { lamports: 100_000_000 },
            mirror_sells: true,
            stop_loss_bp: None,
            take_profit_bp: None,
            min_target_buy_lamports: 50_000_000,
        }
    }
}

/// `lamports / token_units`, exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub lamports: u64,
    pub token_units: u64,
}

impl Price {
    fn of(ev: &SwapEvent) -> Option<Price> {
        (ev.sol_amount > 0 && ev.token_amount > 0).then_some(Price {
            lamports: ev.sol_amount,
            token_units: ev.token_amount,
        })
    }

    /// Tokens bought with `lamports` at this price worsened by `slip_bp`.
    fn tokens_for(&self, lamports: u64, slip_bp: u32) -> u64 {
        let n = lamports as u128 * self.token_units as u128 * BP;
        let d = self.lamports as u128 * (BP + slip_bp as u128);
        u64::try_from(n / d).unwrap_or(u64::MAX)
    }

    /// Lamports received for `tokens` at this price worsened by `slip_bp`.
    fn proceeds_for(&self, tokens: u64, slip_bp: u32) -> u64 {
        let n = tokens as u128 * self.lamports as u128 * (BP - (slip_bp as u128).min(BP));
        let d = self.token_units as u128 * BP;
        u64::try_from(n / d).unwrap_or(u64::MAX)
    }

    /// Mark `tokens` at this price (no slippage).
    fn value_of(&self, tokens: u64) -> u128 {
        tokens as u128 * self.lamports as u128 / self.token_units as u128
    }

    fn lt(&self, other: &Price) -> bool {
        // self < other  ⇔  self.l / self.t < other.l / other.t
        (self.lamports as u128 * other.token_units as u128) < (other.lamports as u128 * self.token_units as u128)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingBuy {
    fill_time: Option<i64>,
    tokens: u64,
    cost: u64,
    price: Price,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimPosition {
    pub mint: String,
    pub token_decimals: u8,
    /// settled tokens we hold
    pub qty: u64,
    pub cost_lamports: u128,
    pub realized_lamports: i128,
    pub buys: u32,
    pub sells: u32,
    pub last_price: Option<Price>,
    pub adverse_entries: u32,
    pub missed_exits: u32,
    pub stopped_out: u32,
    pub took_profit: u32,
    #[serde(skip)]
    pending: Vec<PendingBuy>,
    /// the target's own holdings, replayed, to size mirrored sells
    #[serde(skip)]
    target_qty: u64,
    /// entry price awaiting an adverse-move check at the next trade
    #[serde(skip)]
    watch_entry: Option<Price>,
}

impl SimPosition {
    fn new(mint: &str, decimals: u8) -> Self {
        Self {
            mint: mint.to_string(),
            token_decimals: decimals,
            qty: 0,
            cost_lamports: 0,
            realized_lamports: 0,
            buys: 0,
            sells: 0,
            last_price: None,
            adverse_entries: 0,
            missed_exits: 0,
            stopped_out: 0,
            took_profit: 0,
            pending: Vec::new(),
            target_qty: 0,
            watch_entry: None,
        }
    }

    pub fn unrealized_lamports(&self, slip_bp: u32) -> i128 {
        match self.last_price {
            Some(p) if self.qty > 0 => p.proceeds_for(self.qty, slip_bp) as i128 - self.cost_lamports as i128,
            _ => 0,
        }
    }

    /// Move pending buys whose fill time has passed into the position.
    fn settle(&mut self, now: Option<i64>) {
        let (ready, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| match (p.fill_time, now) {
                (Some(f), Some(n)) => f <= n,
                _ => true,
            });
        for p in ready {
            self.qty = self.qty.saturating_add(p.tokens);
            self.cost_lamports += p.cost as u128;
            self.buys += 1;
            self.watch_entry = Some(p.price);
        }
        self.pending = later;
    }

    fn sell(&mut self, tokens: u64, price: Price, slip_bp: u32) -> u64 {
        let tokens = tokens.min(self.qty);
        if tokens == 0 {
            return 0;
        }
        let proceeds = price.proceeds_for(tokens, slip_bp);
        let cost_removed = self.cost_lamports * tokens as u128 / self.qty as u128;
        self.realized_lamports += proceeds as i128 - cost_removed as i128;
        self.qty -= tokens;
        self.cost_lamports -= cost_removed;
        if self.qty == 0 {
            self.cost_lamports = 0;
        }
        self.sells += 1;
        proceeds
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimResult {
    pub config: SimConfig,
    pub copied_buys: u32,
    pub copied_sells: u32,
    pub skipped_small_buys: u32,
    /// total SOL we would have spent / received
    pub deployed_lamports: u128,
    pub returned_lamports: u128,
    pub realized_lamports: i128,
    pub unrealized_lamports: i128,
    pub open_positions: u32,
    pub wins: u32,
    pub losses: u32,
    pub adverse_entries: u32,
    pub missed_exits: u32,
    pub stopped_out: u32,
    pub took_profit: u32,
    pub positions: BTreeMap<String, SimPosition>,
}

impl SimResult {
    pub fn win_rate_bp(&self) -> Option<u32> {
        let d = self.wins + self.losses;
        (d > 0).then(|| self.wins * 10_000 / d)
    }
}

/// Replay `swaps` (one wallet, `(slot, ix_index)` order) as a copier.
pub fn simulate(cfg: &SimConfig, swaps: &[SwapEvent]) -> SimResult {
    let slip = cfg.slippage_bp;
    let mut positions: BTreeMap<String, SimPosition> = BTreeMap::new();
    let mut copied_buys = 0;
    let mut copied_sells = 0;
    let mut skipped_small = 0;
    let mut deployed: u128 = 0;
    let mut returned: u128 = 0;

    for ev in swaps {
        let Some(price) = Price::of(ev) else { continue };
        let pos = positions
            .entry(ev.mint.clone())
            .or_insert_with(|| SimPosition::new(&ev.mint, ev.token_decimals));
        pos.last_price = Some(price);

        // 1. fills that have matured by now
        pos.settle(ev.block_time);

        // 2. adverse-entry check against this (next observed) price
        if let Some(entry) = pos.watch_entry.take() {
            if price.lt(&entry) {
                pos.adverse_entries += 1;
            }
        }

        // 3. risk exits on our settled position, evaluated at this price
        if pos.qty > 0 {
            let mark = price.value_of(pos.qty);
            let cost = pos.cost_lamports;
            let take = cfg
                .take_profit_bp
                .map_or(false, |tp| mark * BP >= cost * (BP + tp as u128));
            let stop = cfg
                .stop_loss_bp
                .map_or(false, |sl| mark * BP <= cost.saturating_mul(BP.saturating_sub(sl as u128)));
            if take || stop {
                let got = pos.sell(pos.qty, price, slip);
                returned += got as u128;
                copied_sells += 1;
                if take {
                    pos.took_profit += 1;
                } else {
                    pos.stopped_out += 1;
                }
            }
        }

        // 4. mirror the target
        match ev.side {
            Side::Buy => {
                let before = pos.target_qty;
                pos.target_qty = before.saturating_add(ev.token_amount);
                if ev.sol_amount < cfg.min_target_buy_lamports {
                    skipped_small += 1;
                    continue;
                }
                let spend = match cfg.sizing {
                    Sizing::Fixed { lamports } => lamports,
                    Sizing::Fraction { bp, cap_lamports } => {
                        let f = (ev.sol_amount as u128 * bp as u128 / BP) as u64;
                        f.min(cap_lamports)
                    }
                };
                if spend == 0 {
                    continue;
                }
                let tokens = price.tokens_for(spend, slip);
                if tokens == 0 {
                    continue;
                }
                deployed += spend as u128;
                copied_buys += 1;
                pos.pending.push(PendingBuy {
                    fill_time: ev.block_time.map(|t| t + cfg.delay_secs as i64),
                    tokens,
                    cost: spend,
                    price,
                });
            }
            Side::Sell => {
                let before = pos.target_qty;
                pos.target_qty = before.saturating_sub(ev.token_amount);
                if !cfg.mirror_sells {
                    continue;
                }
                // fraction of their book they sold → same fraction of ours
                let ours = if before == 0 || ev.token_amount >= before {
                    pos.qty
                } else {
                    (pos.qty as u128 * ev.token_amount as u128 / before as u128) as u64
                };
                if ours == 0 {
                    if !pos.pending.is_empty() {
                        // they exited before our buy even filled
                        pos.missed_exits += 1;
                    }
                    continue;
                }
                let got = pos.sell(ours, price, slip);
                returned += got as u128;
                copied_sells += 1;
            }
        }
    }

    // anything still pending fills at the end (we're holding it)
    for pos in positions.values_mut() {
        pos.settle(None);
    }

    let mut wins = 0;
    let mut losses = 0;
    for p in positions.values() {
        if p.sells > 0 {
            if p.realized_lamports > 0 {
                wins += 1;
            } else if p.realized_lamports < 0 {
                losses += 1;
            }
        }
    }
    SimResult {
        config: cfg.clone(),
        copied_buys,
        copied_sells,
        skipped_small_buys: skipped_small,
        deployed_lamports: deployed,
        returned_lamports: returned,
        realized_lamports: positions.values().map(|p| p.realized_lamports).sum(),
        unrealized_lamports: positions.values().map(|p| p.unrealized_lamports(slip)).sum(),
        open_positions: positions.values().filter(|p| p.qty > 0).count() as u32,
        wins,
        losses,
        adverse_entries: positions.values().map(|p| p.adverse_entries).sum(),
        missed_exits: positions.values().map(|p| p.missed_exits).sum(),
        stopped_out: positions.values().map(|p| p.stopped_out).sum(),
        took_profit: positions.values().map(|p| p.took_profit).sum(),
        positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decode::Venue;

    const SOL: u64 = 1_000_000_000;

    fn ev(t: i64, mint: &str, side: Side, tokens: u64, lamports: u64) -> SwapEvent {
        SwapEvent {
            signature: format!("sig{t}"),
            slot: t as u64,
            block_time: Some(t),
            wallet: "W".into(),
            mint: mint.into(),
            venue: Venue::PumpFunCurve,
            side,
            token_amount: tokens,
            token_decimals: 6,
            sol_amount: lamports,
            fee_lamports: 0,
            ix_index: 0,
        }
    }

    fn cfg() -> SimConfig {
        SimConfig {
            delay_secs: 15,
            slippage_bp: 200,
            sizing: Sizing::Fixed { lamports: SOL / 10 },
            mirror_sells: true,
            stop_loss_bp: None,
            take_profit_bp: None,
            min_target_buy_lamports: 0,
        }
    }

    /// Target buys 1000 tokens for 1 SOL, sells all for 2 SOL a minute later.
    /// We spend 0.1 SOL at 1.02× their price → 98,039,215 micro-tokens…
    /// (tokens are raw units here: 1000 units): 0.1 SOL × 1000 / 1.02 = 98 units.
    /// We sell 98 units at 2 SOL/1000 × 0.98 = 192,080,000 lamports.
    /// Realized = 192,080,000 − 100,000,000 = +92,080,000.
    #[test]
    fn buy_then_full_exit_hand_verified() {
        let swaps = [ev(0, "M", Side::Buy, 1000, SOL), ev(60, "M", Side::Sell, 1000, 2 * SOL)];
        let r = simulate(&cfg(), &swaps);
        let p = &r.positions["M"];
        assert_eq!(r.copied_buys, 1);
        assert_eq!(r.copied_sells, 1);
        assert_eq!(p.qty, 0);
        assert_eq!(r.deployed_lamports, SOL as u128 / 10);
        assert_eq!(r.returned_lamports, 192_080_000);
        assert_eq!(r.realized_lamports, 92_080_000);
        assert_eq!(r.wins, 1);
        assert_eq!(r.win_rate_bp(), Some(10_000));
        assert_eq!(r.adverse_entries, 0); // price went up after our entry
    }

    #[test]
    fn target_exit_inside_the_delay_is_missed_and_we_hold_the_bag() {
        // sell 5 s after buy; our fill is at +15 s → nothing to mirror
        let swaps = [ev(0, "M", Side::Buy, 1000, SOL), ev(5, "M", Side::Sell, 1000, 2 * SOL)];
        let r = simulate(&cfg(), &swaps);
        let p = &r.positions["M"];
        assert_eq!(r.missed_exits, 1);
        assert_eq!(r.copied_sells, 0);
        assert_eq!(p.qty, 98); // filled at the end, still held
        assert_eq!(r.open_positions, 1);
        // marked at the last price (2 SOL/1000) with slippage: 98 × 2e9/1000 × 0.98 = 192,080,000
        assert_eq!(r.unrealized_lamports, 192_080_000 - 100_000_000);
    }

    #[test]
    fn partial_sell_mirrors_fraction_and_adverse_entry_is_counted() {
        let swaps = [
            ev(0, "M", Side::Buy, 1000, SOL),
            ev(60, "M", Side::Sell, 500, 250_000_000), // they dump half at 0.5 SOL/1000 — below our entry
            ev(120, "M", Side::Sell, 500, 250_000_000),
        ];
        let r = simulate(&cfg(), &swaps);
        let p = &r.positions["M"];
        assert_eq!(r.adverse_entries, 1);
        assert_eq!(r.copied_sells, 2);
        assert_eq!(p.qty, 0);
        // first sell: 49 units at 0.5e9/1000 × 0.98 = 24,010,000; second: 49 units → 24,010,000
        assert_eq!(r.returned_lamports, 48_020_000);
        assert_eq!(r.realized_lamports, 48_020_000 - 100_000_000);
        assert_eq!(r.losses, 1);
    }

    #[test]
    fn stop_loss_exits_at_the_observed_price() {
        let mut c = cfg();
        c.stop_loss_bp = Some(3_000); // −30%
        let swaps = [
            ev(0, "M", Side::Buy, 1000, SOL),
            ev(60, "M", Side::Buy, 1000, 500_000_000), // target adds at half price: our mark is −51%
            ev(120, "M", Side::Sell, 2000, 4 * SOL),   // later moon — we already stopped out
        ];
        let r = simulate(&c, &swaps);
        assert_eq!(r.stopped_out, 1);
        assert_eq!(r.copied_buys, 2); // we also copied the second buy after stopping out
        let p = &r.positions["M"];
        // stop: 98 units at 0.5e9/1000 × 0.98 = 48,020,000 against 100,000,000 cost → −51,980,000
        // second copy: 196 units, mirrored exit at 4e9/2000 × 0.98 = 384,160,000 → +284,160,000
        assert_eq!(p.realized_lamports, -51_980_000 + 284_160_000);
        assert_eq!(r.adverse_entries, 1); // the first entry was followed by a lower price
        assert_eq!(p.qty, 0);
    }

    #[test]
    fn fraction_sizing_is_capped_and_small_buys_are_skipped() {
        let mut c = cfg();
        c.sizing = Sizing::Fraction { bp: 5_000, cap_lamports: 200_000_000 };
        c.min_target_buy_lamports = SOL / 2;
        let swaps = [
            ev(0, "A", Side::Buy, 1000, SOL / 10), // too small: skipped
            ev(1, "B", Side::Buy, 1000, SOL),      // 50% = 0.5 SOL, capped to 0.2
        ];
        let r = simulate(&c, &swaps);
        assert_eq!(r.skipped_small_buys, 1);
        assert_eq!(r.copied_buys, 1);
        assert_eq!(r.deployed_lamports, 200_000_000);
    }

    #[test]
    fn no_block_time_means_immediate_fill() {
        let mut a = ev(0, "M", Side::Buy, 1000, SOL);
        a.block_time = None;
        let mut b = ev(1, "M", Side::Sell, 1000, 2 * SOL);
        b.block_time = None;
        let r = simulate(&cfg(), &[a, b]);
        assert_eq!(r.copied_sells, 1);
        assert_eq!(r.missed_exits, 0);
    }
}
