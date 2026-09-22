//! Token-centric aggregation across *all* tracked wallets: who is buying what,
//! how much SOL is flowing in, and the "confluence" signal — several vetted
//! wallets buying the same mint inside a short window.

use std::collections::{BTreeMap, BTreeSet};

use decode::{Side, SwapEvent};
use serde::{Deserialize, Serialize};

use crate::{Price, WalletPnl};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenActivity {
    pub mint: String,
    pub token_decimals: u8,
    /// distinct tracked wallets that bought / sold in the window
    pub buyers: BTreeSet<String>,
    pub sellers: BTreeSet<String>,
    pub buys: u32,
    pub sells: u32,
    pub bought_lamports: u128,
    pub sold_lamports: u128,
    pub first_trade: Option<i64>,
    pub last_trade: Option<i64>,
    pub last_price: Option<Price>,
    pub last_venue: Option<decode::Venue>,
}

impl TokenActivity {
    pub fn net_flow_lamports(&self) -> i128 {
        self.bought_lamports as i128 - self.sold_lamports as i128
    }
}

/// Aggregate swaps (any wallets, any order) with `block_time >= since`.
pub fn token_activity(swaps: &[SwapEvent], since: i64) -> BTreeMap<String, TokenActivity> {
    let mut map: BTreeMap<String, TokenActivity> = BTreeMap::new();
    let mut ordered: Vec<&SwapEvent> = swaps
        .iter()
        .filter(|s| s.block_time.map_or(false, |t| t >= since))
        .collect();
    ordered.sort_by_key(|s| (s.slot, s.ix_index));
    for s in ordered {
        let a = map.entry(s.mint.clone()).or_insert_with(|| TokenActivity {
            mint: s.mint.clone(),
            token_decimals: s.token_decimals,
            buyers: BTreeSet::new(),
            sellers: BTreeSet::new(),
            buys: 0,
            sells: 0,
            bought_lamports: 0,
            sold_lamports: 0,
            first_trade: None,
            last_trade: None,
            last_price: None,
            last_venue: None,
        });
        match s.side {
            Side::Buy => {
                a.buyers.insert(s.wallet.clone());
                a.buys += 1;
                a.bought_lamports += s.sol_amount as u128;
            }
            Side::Sell => {
                a.sellers.insert(s.wallet.clone());
                a.sells += 1;
                a.sold_lamports += s.sol_amount as u128;
            }
        }
        if a.first_trade.map_or(true, |t| s.block_time.unwrap_or(t) < t) {
            a.first_trade = s.block_time;
        }
        a.last_trade = s.block_time;
        if s.token_amount > 0 {
            a.last_price = Some(Price {
                lamports: s.sol_amount,
                token_units: s.token_amount,
            });
        }
        a.last_venue = Some(s.venue);
    }
    map
}

/// What the tracked wallets currently hold in a mint, summed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TokenHolding {
    pub holders: u32,
    pub qty: u64,
    pub cost_lamports: u128,
    pub value_lamports: u128,
    pub realized_lamports: i128,
    pub unrealized_lamports: i128,
}

pub fn token_holdings<'a>(wallets: impl Iterator<Item = &'a WalletPnl>) -> BTreeMap<String, TokenHolding> {
    let mut map: BTreeMap<String, TokenHolding> = BTreeMap::new();
    for w in wallets {
        for p in w.positions.values() {
            let h = map.entry(p.mint.clone()).or_default();
            if p.qty > 0 {
                h.holders += 1;
                h.qty = h.qty.saturating_add(p.qty);
                h.cost_lamports += p.cost_lamports;
                h.value_lamports += p.value_lamports();
                h.unrealized_lamports += p.unrealized_lamports();
            }
            h.realized_lamports += p.realized_lamports;
        }
    }
    map
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfluenceRule {
    pub min_wallets: u32,
    pub window_secs: i64,
}

impl Default for ConfluenceRule {
    fn default() -> Self {
        Self {
            min_wallets: 3,
            window_secs: 30 * 60,
        }
    }
}

/// Distinct tracked wallets that bought `mint` within `[now - window, now]`,
/// if there are at least `min_wallets` of them. `swaps` may contain other
/// mints and sides; only buys of `mint` count.
pub fn confluence(swaps: &[SwapEvent], mint: &str, rule: ConfluenceRule, now: i64) -> Option<Vec<String>> {
    let cutoff = now - rule.window_secs;
    let wallets: BTreeSet<&str> = swaps
        .iter()
        .filter(|s| s.mint == mint && s.side == Side::Buy && s.block_time.map_or(false, |t| t >= cutoff && t <= now))
        .map(|s| s.wallet.as_str())
        .collect();
    (wallets.len() as u32 >= rule.min_wallets.max(1)).then(|| wallets.into_iter().map(str::to_string).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use decode::Venue;

    fn ev(t: i64, wallet: &str, mint: &str, side: Side, lamports: u64) -> SwapEvent {
        SwapEvent {
            signature: format!("s{t}{wallet}"),
            slot: t as u64,
            block_time: Some(t),
            wallet: wallet.into(),
            mint: mint.into(),
            venue: Venue::PumpFunCurve,
            side,
            token_amount: 1_000,
            token_decimals: 6,
            sol_amount: lamports,
            fee_lamports: 0,
            ix_index: 0,
        }
    }

    #[test]
    fn activity_aggregates_across_wallets() {
        let swaps = [
            ev(100, "A", "M", Side::Buy, 10),
            ev(110, "B", "M", Side::Buy, 20),
            ev(120, "A", "M", Side::Sell, 5),
            ev(50, "C", "M", Side::Buy, 99), // before `since`
            ev(130, "A", "N", Side::Buy, 7),
        ];
        let act = token_activity(&swaps, 100);
        let m = &act["M"];
        assert_eq!(m.buyers.len(), 2);
        assert_eq!(m.sellers.len(), 1);
        assert_eq!((m.buys, m.sells), (2, 1));
        assert_eq!(m.bought_lamports, 30);
        assert_eq!(m.sold_lamports, 5);
        assert_eq!(m.net_flow_lamports(), 25);
        assert_eq!(m.first_trade, Some(100));
        assert_eq!(m.last_trade, Some(120));
        assert_eq!(act["N"].buyers.len(), 1);
    }

    #[test]
    fn confluence_needs_distinct_wallets_inside_the_window() {
        let rule = ConfluenceRule {
            min_wallets: 3,
            window_secs: 600,
        };
        let swaps = [
            ev(1000, "A", "M", Side::Buy, 1),
            ev(1100, "A", "M", Side::Buy, 1), // same wallet again: doesn't count twice
            ev(1200, "B", "M", Side::Buy, 1),
            ev(1300, "C", "N", Side::Buy, 1), // other mint
            ev(1400, "C", "M", Side::Sell, 1), // a sell doesn't count
        ];
        assert_eq!(confluence(&swaps, "M", rule, 1450), None);
        let mut with_c = swaps.to_vec();
        with_c.push(ev(1450, "C", "M", Side::Buy, 1));
        assert_eq!(confluence(&with_c, "M", rule, 1450), Some(vec!["A".into(), "B".into(), "C".into()]));
        // window slides: A's buys at 1000/1100 fall out by 1750
        assert_eq!(confluence(&with_c, "M", rule, 1750), None);
    }

    #[test]
    fn holdings_sum_over_wallets() {
        let a = WalletPnl::compute("A", &[ev(1, "A", "M", Side::Buy, 100)]);
        let b = WalletPnl::compute("B", &[ev(2, "B", "M", Side::Buy, 50), ev(3, "B", "M", Side::Sell, 80)]);
        let h = token_holdings([&a, &b].into_iter());
        let m = &h["M"];
        assert_eq!(m.holders, 1);
        assert_eq!(m.qty, 1_000);
        assert_eq!(m.cost_lamports, 100);
        assert_eq!(m.realized_lamports, 30);
    }
}
