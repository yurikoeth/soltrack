//! Wire types for the webview. Amounts cross as decimal strings: JS numbers
//! lose precision above 2^53 and raw token units get there easily.

use decode::{SwapEvent, TokenMeta, UnknownSwap};
use pnl::tokens::{TokenActivity, TokenHolding};
use pnl::{WalletPnl, WalletStats};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct SwapRow {
    pub signature: String,
    pub ix_index: u16,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub wallet: String,
    pub mint: String,
    pub venue: &'static str,
    pub side: &'static str,
    pub token_amount: String,
    pub token_decimals: u8,
    pub sol_amount: String,
    pub fee_lamports: String,
}

impl From<&SwapEvent> for SwapRow {
    fn from(e: &SwapEvent) -> Self {
        Self {
            signature: e.signature.clone(),
            ix_index: e.ix_index,
            slot: e.slot,
            block_time: e.block_time,
            wallet: e.wallet.clone(),
            mint: e.mint.clone(),
            venue: e.venue.as_str(),
            side: e.side.as_str(),
            token_amount: e.token_amount.to_string(),
            token_decimals: e.token_decimals,
            sol_amount: e.sol_amount.to_string(),
            fee_lamports: e.fee_lamports.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UnknownRow {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub wallet: String,
    pub programs: Vec<String>,
}

impl From<&UnknownSwap> for UnknownRow {
    fn from(u: &UnknownSwap) -> Self {
        Self {
            signature: u.signature.clone(),
            slot: u.slot,
            block_time: u.block_time,
            wallet: u.wallet.clone(),
            programs: u.programs.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionRow {
    pub mint: String,
    pub token_decimals: u8,
    pub qty: String,
    pub cost_lamports: String,
    pub value_lamports: String,
    pub realized_lamports: String,
    pub unrealized_lamports: String,
    pub buys: u32,
    pub sells: u32,
    pub hold_secs: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsRow {
    pub trades: u32,
    pub buys: u32,
    pub sells: u32,
    pub tokens_traded: u32,
    pub tokens_with_exits: u32,
    pub wins: u32,
    pub losses: u32,
    pub win_rate_bp: Option<u32>,
    pub median_hold_secs: Option<i64>,
    pub median_buy_lamports: Option<String>,
    pub max_buy_lamports: String,
    pub bought_lamports: String,
    pub sold_lamports: String,
    pub realized_24h: String,
    pub realized_7d: String,
    pub realized_30d: String,
    pub oversold_events: u32,
}

impl From<&WalletStats> for StatsRow {
    fn from(s: &WalletStats) -> Self {
        Self {
            trades: s.trades,
            buys: s.buys,
            sells: s.sells,
            tokens_traded: s.tokens_traded,
            tokens_with_exits: s.tokens_with_exits,
            wins: s.wins,
            losses: s.losses,
            win_rate_bp: s.win_rate_bp,
            median_hold_secs: s.median_hold_secs,
            median_buy_lamports: s.median_buy_lamports.map(|v| v.to_string()),
            max_buy_lamports: s.max_buy_lamports.to_string(),
            bought_lamports: s.bought_lamports.to_string(),
            sold_lamports: s.sold_lamports.to_string(),
            realized_24h: s.realized_24h.to_string(),
            realized_7d: s.realized_7d.to_string(),
            realized_30d: s.realized_30d.to_string(),
            oversold_events: s.oversold_events,
        }
    }
}

/// Paper-trading outcome for one wallet.
#[derive(Debug, Clone, Serialize)]
pub struct SimRow {
    pub describe: String,
    pub copied_buys: u32,
    pub copied_sells: u32,
    pub skipped_small_buys: u32,
    pub deployed_lamports: String,
    pub returned_lamports: String,
    pub realized_lamports: String,
    pub unrealized_lamports: String,
    pub open_positions: u32,
    pub wins: u32,
    pub losses: u32,
    pub win_rate_bp: Option<u32>,
    pub adverse_entries: u32,
    pub missed_exits: u32,
    pub stopped_out: u32,
    pub took_profit: u32,
}

impl SimRow {
    pub fn new(r: &sim::SimResult, describe: &str) -> Self {
        Self {
            describe: describe.to_string(),
            copied_buys: r.copied_buys,
            copied_sells: r.copied_sells,
            skipped_small_buys: r.skipped_small_buys,
            deployed_lamports: r.deployed_lamports.to_string(),
            returned_lamports: r.returned_lamports.to_string(),
            realized_lamports: r.realized_lamports.to_string(),
            unrealized_lamports: r.unrealized_lamports.to_string(),
            open_positions: r.open_positions,
            wins: r.wins,
            losses: r.losses,
            win_rate_bp: r.win_rate_bp(),
            adverse_entries: r.adverse_entries,
            missed_exits: r.missed_exits,
            stopped_out: r.stopped_out,
            took_profit: r.took_profit,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletRow {
    pub wallet: String,
    pub label: Option<String>,
    pub last_trade: Option<SwapRow>,
    pub realized_lamports: String,
    pub unrealized_lamports: String,
    pub trade_count: u32,
    pub stats: StatsRow,
    pub sim: Option<SimRow>,
    pub open_positions: Vec<PositionRow>,
    pub closed_positions: Vec<PositionRow>,
}

impl WalletRow {
    pub fn new(w: &WalletPnl, label: Option<&str>, sim: Option<SimRow>, now: i64) -> Self {
        let row = |p: &pnl::Position| PositionRow {
            mint: p.mint.clone(),
            token_decimals: p.token_decimals,
            qty: p.qty.to_string(),
            cost_lamports: p.cost_lamports.to_string(),
            value_lamports: p.value_lamports().to_string(),
            realized_lamports: p.realized_lamports.to_string(),
            unrealized_lamports: p.unrealized_lamports().to_string(),
            buys: p.buys,
            sells: p.sells,
            hold_secs: p.hold_secs(),
        };
        Self {
            wallet: w.wallet.clone(),
            label: label.map(str::to_string),
            last_trade: w.last_trade.as_ref().map(SwapRow::from),
            realized_lamports: w.realized_lamports().to_string(),
            unrealized_lamports: w.unrealized_lamports().to_string(),
            trade_count: w.trade_count,
            stats: StatsRow::from(&w.stats(now)),
            sim,
            open_positions: w.open_positions().map(row).collect(),
            closed_positions: w.positions.values().filter(|p| p.qty == 0).map(row).collect(),
        }
    }
}

/// Token metadata as the webview sees it: the remote image URL plus, when we
/// have downloaded it, the local cache file (served via the asset protocol).
#[derive(Debug, Clone, Serialize)]
pub struct MetaRow {
    pub mint: String,
    pub symbol: String,
    pub name: String,
    pub uri: String,
    pub image: String,
    pub local: Option<String>,
}

impl MetaRow {
    pub fn new(m: &TokenMeta, local: Option<std::path::PathBuf>) -> Self {
        Self {
            mint: m.mint.clone(),
            symbol: m.symbol.clone(),
            name: m.name.clone(),
            uri: m.uri.clone(),
            image: m.image.clone(),
            local: local.map(|p| p.display().to_string()),
        }
    }
}

/// `buy` | `big_buy` | `sell` | `confluence` | `migrated` | `progress` | `mcap`
#[derive(Debug, Clone, Serialize)]
pub struct AlertRow {
    pub id: u64,
    pub kind: &'static str,
    /// unix secs when we raised it
    pub at: i64,
    pub mint: String,
    /// the wallet involved, for swap alerts
    pub wallet: Option<String>,
    pub label: Option<String>,
    pub swap: Option<SwapRow>,
    /// the wallet's median buy at the time, for context on big buys
    pub median_buy_lamports: Option<String>,
    /// human context: confluence wallets, mcap reached, …
    pub note: Option<String>,
}

/// An early buyer found by discovery.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateRow {
    pub rank: u32,
    pub wallet: String,
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub slots_after_launch: u64,
    pub venue: &'static str,
    pub sol_amount: String,
    pub token_amount: String,
    pub token_decimals: u8,
    pub extra_buys: u32,
    pub sold_in_window: bool,
    pub tracked: bool,
}

impl CandidateRow {
    pub fn new(c: &ingest::Candidate, tracked: bool) -> Self {
        Self {
            rank: c.rank,
            wallet: c.wallet.clone(),
            signature: c.signature.clone(),
            slot: c.slot,
            block_time: c.block_time,
            slots_after_launch: c.slots_after_launch,
            venue: c.venue,
            sol_amount: c.sol_amount.clone(),
            token_amount: c.token_amount.clone(),
            token_decimals: c.token_decimals,
            extra_buys: c.extra_buys,
            sold_in_window: c.sold_in_window,
            tracked,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoverResult {
    pub mint: String,
    pub candidates: Vec<CandidateRow>,
    pub reached_launch: bool,
    pub scanned: u32,
    pub launch_slot: u64,
    pub launch_time: Option<i64>,
    pub max_pages: u32,
}

/// On-chain state of a token as last fetched.
#[derive(Debug, Clone, Serialize)]
pub struct TokenStateRow {
    pub fetched_at: i64,
    pub is_pump: bool,
    pub complete: bool,
    pub progress_bp: u32,
    pub mcap_lamports: Option<String>,
    pub real_sol_lamports: Option<String>,
    pub price_lamports: Option<String>,
    pub price_token_units: Option<String>,
    pub pool: Option<String>,
    pub creator: Option<String>,
}

impl From<&ingest::TokenState> for TokenStateRow {
    fn from(s: &ingest::TokenState) -> Self {
        let price = s.price();
        Self {
            fetched_at: s.fetched_at,
            is_pump: s.curve.is_some(),
            complete: s.curve.as_ref().map_or(false, |c| c.complete),
            progress_bp: s.curve.as_ref().map_or(0, |c| c.progress_bp),
            mcap_lamports: s.mcap_lamports().map(|m| m.to_string()),
            real_sol_lamports: s.curve.as_ref().map(|c| c.real_sol_lamports.to_string()),
            price_lamports: price.map(|(l, _)| l.to_string()),
            price_token_units: price.map(|(_, t)| t.to_string()),
            pool: s.pool.as_ref().map(|p| p.pool.clone()),
            creator: s.curve.as_ref().and_then(|c| c.creator.clone()),
        }
    }
}

/// One row of the tokens tab: tracked-wallet activity on a mint.
#[derive(Debug, Clone, Serialize)]
pub struct TokenRow {
    pub mint: String,
    pub token_decimals: u8,
    pub buyers: Vec<String>,
    pub sellers: Vec<String>,
    pub buys: u32,
    pub sells: u32,
    pub bought_lamports: String,
    pub sold_lamports: String,
    pub net_flow_lamports: String,
    pub first_trade: Option<i64>,
    pub last_trade: Option<i64>,
    pub last_venue: Option<&'static str>,
    pub last_price_lamports: Option<String>,
    pub last_price_token_units: Option<String>,
    pub holders: u32,
    pub held_qty: String,
    pub held_value_lamports: String,
    pub realized_lamports: String,
    pub unrealized_lamports: String,
    pub state: Option<TokenStateRow>,
    pub watched: bool,
}

impl TokenRow {
    pub fn new(a: &TokenActivity, h: Option<&TokenHolding>, state: Option<&ingest::TokenState>, watched: bool) -> Self {
        let h = h.cloned().unwrap_or_default();
        Self {
            mint: a.mint.clone(),
            token_decimals: a.token_decimals,
            buyers: a.buyers.iter().cloned().collect(),
            sellers: a.sellers.iter().cloned().collect(),
            buys: a.buys,
            sells: a.sells,
            bought_lamports: a.bought_lamports.to_string(),
            sold_lamports: a.sold_lamports.to_string(),
            net_flow_lamports: a.net_flow_lamports().to_string(),
            first_trade: a.first_trade,
            last_trade: a.last_trade,
            last_venue: a.last_venue.map(|v| v.as_str()),
            last_price_lamports: a.last_price.map(|p| p.lamports.to_string()),
            last_price_token_units: a.last_price.map(|p| p.token_units.to_string()),
            holders: h.holders,
            held_qty: h.qty.to_string(),
            held_value_lamports: h.value_lamports.to_string(),
            realized_lamports: h.realized_lamports.to_string(),
            unrealized_lamports: h.unrealized_lamports.to_string(),
            state: state.map(TokenStateRow::from),
            watched,
        }
    }
}

/// One polled sample of a watched token.
#[derive(Debug, Clone, Serialize)]
pub struct WatchPoint {
    pub t: i64,
    pub price_lamports: Option<String>,
    pub price_token_units: Option<String>,
    pub mcap_lamports: Option<String>,
    pub progress_bp: u32,
    pub complete: bool,
    pub is_pump: bool,
}

impl From<&ingest::TokenState> for WatchPoint {
    fn from(s: &ingest::TokenState) -> Self {
        let price = s.price();
        Self {
            t: s.fetched_at,
            price_lamports: price.map(|(l, _)| l.to_string()),
            price_token_units: price.map(|(_, t)| t.to_string()),
            mcap_lamports: s.mcap_lamports().map(|m| m.to_string()),
            progress_bp: s.curve.as_ref().map_or(0, |c| c.progress_bp),
            complete: s.curve.as_ref().map_or(false, |c| c.complete),
            is_pump: s.curve.is_some(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchRow {
    pub mint: String,
    pub points: Vec<WatchPoint>,
    pub latest: Option<WatchPoint>,
    pub creator: Option<String>,
}

/// A new Pump.fun token.
#[derive(Debug, Clone, Serialize)]
pub struct LaunchRow {
    pub mint: String,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub creator: String,
    pub creator_tracked: bool,
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub dev_buy_lamports: String,
    pub dev_buy_tokens: String,
    pub token_total_supply: String,
    pub initial_mcap_lamports: String,
    pub watched: bool,
}

impl LaunchRow {
    pub fn new(l: &ingest::Launch, creator_tracked: bool, watched: bool) -> Self {
        Self {
            mint: l.mint.clone(),
            name: l.name.clone(),
            symbol: l.symbol.clone(),
            uri: l.uri.clone(),
            creator: l.creator.clone(),
            creator_tracked,
            signature: l.signature.clone(),
            slot: l.slot,
            block_time: l.block_time,
            dev_buy_lamports: l.dev_buy_lamports.to_string(),
            dev_buy_tokens: l.dev_buy_tokens.to_string(),
            token_total_supply: l.token_total_supply.to_string(),
            initial_mcap_lamports: l.initial_mcap_lamports.to_string(),
            watched,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub wallets: Vec<WalletRow>,
    pub feed: Vec<SwapRow>,
    pub unknown: Vec<UnknownRow>,
    pub alerts: Vec<AlertRow>,
    pub meta: Vec<MetaRow>,
    pub status: ingest::ConnState,
    pub config_path: String,
    pub config_created: bool,
    pub config_error: Option<String>,
    pub rpc_url: String,
    pub fetch_failures: u64,
    pub alerts_enabled: bool,
    pub sound: bool,
    pub muted: bool,
    pub sim_enabled: bool,
    pub sim_describe: String,
    pub tokens_window_hours: u32,
    pub confluence_wallets: u32,
    pub confluence_minutes: u32,
    pub watch: Vec<WatchRow>,
    pub launches: Vec<LaunchRow>,
    pub launches_enabled: bool,
    pub launch_state: ingest::LaunchState,
    pub chart_url: String,
}
