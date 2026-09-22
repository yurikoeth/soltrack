//! On-chain state of a token, fetched: bonding curve while on Pump.fun, the
//! canonical PumpSwap pool after migration. One `getAccountInfo` for the
//! curve; if it has completed, one `getMultipleAccounts` for the pool's two
//! vaults (the pool address is derived, its vault addresses are read from
//! the pool account — three calls total, only for migrated tokens).

use decode::curve::{self, BondingCurve};
use serde::Serialize;

use crate::rpc::{RpcClient, RpcError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurveState {
    pub progress_bp: u32,
    pub complete: bool,
    /// lamports per token unit, exact
    pub price_lamports: u64,
    pub price_token_units: u64,
    pub mcap_lamports: String,
    pub real_sol_lamports: u64,
    pub token_total_supply: u64,
    pub creator: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolState {
    pub pool: String,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub mcap_lamports: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenState {
    pub mint: String,
    pub fetched_at: i64,
    /// `None` when the mint has no Pump.fun bonding curve (not a pump token)
    pub curve: Option<CurveState>,
    /// present once migrated and the canonical pool exists
    pub pool: Option<PoolState>,
}

impl TokenState {
    /// Best current price as `(lamports, token_units)`: pool if migrated, else curve.
    pub fn price(&self) -> Option<(u64, u64)> {
        if let Some(p) = &self.pool {
            if p.base_reserve > 0 {
                return Some((p.quote_reserve, p.base_reserve));
            }
        }
        self.curve.as_ref().map(|c| (c.price_lamports, c.price_token_units))
    }

    pub fn mcap_lamports(&self) -> Option<u128> {
        if let Some(p) = &self.pool {
            return p.mcap_lamports.parse().ok();
        }
        self.curve.as_ref().and_then(|c| c.mcap_lamports.parse().ok())
    }
}

fn curve_state(c: &BondingCurve) -> CurveState {
    let (pl, pt) = c.price().unwrap_or((0, 1));
    CurveState {
        progress_bp: c.progress_bp(),
        complete: c.complete,
        price_lamports: pl,
        price_token_units: pt,
        mcap_lamports: c.mcap_lamports().to_string(),
        real_sol_lamports: c.real_sol_reserves,
        token_total_supply: c.token_total_supply,
        creator: c.creator.clone(),
    }
}

pub async fn fetch_token_state(rpc: &RpcClient, mint: &str) -> Result<TokenState, RpcError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut state = TokenState {
        mint: mint.to_string(),
        fetched_at: now,
        curve: None,
        pool: None,
    };
    let Some(curve_pda) = curve::bonding_curve_pda(mint) else {
        return Ok(state);
    };
    let Some((_, data)) = rpc.get_account(&curve_pda).await? else {
        return Ok(state); // not a pump token
    };
    let Some(c) = curve::parse_bonding_curve(&data) else {
        return Ok(state);
    };
    state.curve = Some(curve_state(&c));
    if !c.complete {
        return Ok(state);
    }
    // migrated: read the canonical pool's vaults
    let Some(pool_pda) = curve::canonical_pool_pda(mint) else {
        return Ok(state);
    };
    let Some((_, pool_data)) = rpc.get_account(&pool_pda).await? else {
        return Ok(state);
    };
    let Some(pool) = curve::parse_pool(&pool_data) else {
        return Ok(state);
    };
    let accounts = rpc
        .get_multiple_accounts(&[&pool.pool_base_token_account, &pool.pool_quote_token_account])
        .await?;
    let base = accounts
        .first()
        .and_then(|a| a.as_ref())
        .and_then(|(_, d)| curve::token_account_amount(d))
        .unwrap_or(0);
    let quote = accounts
        .get(1)
        .and_then(|a| a.as_ref())
        .and_then(|(_, d)| curve::token_account_amount(d))
        .unwrap_or(0);
    let mcap = if base > 0 {
        quote as u128 * c.token_total_supply as u128 / base as u128
    } else {
        0
    };
    state.pool = Some(PoolState {
        pool: pool_pda,
        base_reserve: base,
        quote_reserve: quote,
        mcap_lamports: mcap.to_string(),
    });
    Ok(state)
}
