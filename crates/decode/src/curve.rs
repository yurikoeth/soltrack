//! On-chain *state* of a Pump.fun token: the bonding curve account while it
//! is on the curve, the canonical PumpSwap pool after it migrates. Pure
//! parsers plus PDA derivations; fetching lives in `ingest::tokens`.
//!
//! `BondingCurve` account (Anchor, discriminator `account:BondingCurve`):
//! ```text
//!   8  virtual_token_reserves  u64
//!  16  virtual_sol_reserves    u64
//!  24  real_token_reserves     u64
//!  32  real_sol_reserves       u64
//!  40  token_total_supply      u64
//!  48  complete                bool
//!  49  creator                 [u8;32]   (newer curves)
//! ```
//! Price = virtual_sol / virtual_token; market cap = price × total supply.
//! "Progress" is how much of the 793.1M curve allocation has been sold.
//!
//! PumpSwap `Pool` account (`account:Pool`):
//! ```text
//!   8  pool_bump u8 | 9 index u16 | 11 creator | 43 base_mint | 75 quote_mint
//! 107  lp_mint | 139 pool_base_token_account | 171 pool_quote_token_account
//! 203  lp_supply u64 | 211 coin_creator
//! ```
//! Reserves live in the two vault token accounts (SPL layout: amount at 64).

use crate::anchor::{discriminator, Reader};
use crate::pda;
use crate::pumpfun::PROGRAM_ID as PUMP;
use crate::pumpswap::PROGRAM_ID as PUMPSWAP;
use crate::WSOL_MINT;

/// Tokens initially available on the curve (793.1M × 10^6). The remaining
/// 206.9M are reserved for the migration pool.
pub const CURVE_INITIAL_REAL_TOKENS: u64 = 793_100_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondingCurve {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    pub creator: Option<String>,
}

impl BondingCurve {
    /// Share of the curve allocation sold, in basis points (10_000 = graduated).
    pub fn progress_bp(&self) -> u32 {
        if self.complete {
            return 10_000;
        }
        let sold = CURVE_INITIAL_REAL_TOKENS.saturating_sub(self.real_token_reserves) as u128;
        (sold * 10_000 / CURVE_INITIAL_REAL_TOKENS as u128).min(10_000) as u32
    }

    /// Lamports per token unit as an exact rational `(lamports, token_units)`.
    pub fn price(&self) -> Option<(u64, u64)> {
        (self.virtual_token_reserves > 0).then_some((self.virtual_sol_reserves, self.virtual_token_reserves))
    }

    pub fn mcap_lamports(&self) -> u128 {
        if self.virtual_token_reserves == 0 {
            return 0;
        }
        self.virtual_sol_reserves as u128 * self.token_total_supply as u128 / self.virtual_token_reserves as u128
    }
}

pub fn parse_bonding_curve(data: &[u8]) -> Option<BondingCurve> {
    if data.len() < 8 || data[..8] != discriminator("account", "BondingCurve") {
        return None;
    }
    let mut r = Reader::new(&data[8..]);
    let virtual_token_reserves = r.u64()?;
    let virtual_sol_reserves = r.u64()?;
    let real_token_reserves = r.u64()?;
    let real_sol_reserves = r.u64()?;
    let token_total_supply = r.u64()?;
    let complete = r.bool()?;
    let creator = if r.remaining() >= 32 { r.pubkey() } else { None };
    Some(BondingCurve {
        virtual_token_reserves,
        virtual_sol_reserves,
        real_token_reserves,
        real_sol_reserves,
        token_total_supply,
        complete,
        creator,
    })
}

pub fn bonding_curve_pda(mint: &str) -> Option<String> {
    let m = pda::pubkey_bytes(mint)?;
    pda::derive(&[b"bonding-curve", &m], PUMP)
}

/// Pump.fun's per-token authority that owns the migrated PumpSwap pool.
pub fn pool_authority_pda(mint: &str) -> Option<String> {
    let m = pda::pubkey_bytes(mint)?;
    pda::derive(&[b"pool-authority", &m], PUMP)
}

/// The canonical PumpSwap pool created at migration (index 0, WSOL quote).
pub fn canonical_pool_pda(mint: &str) -> Option<String> {
    let authority = pda::pubkey_bytes(&pool_authority_pda(mint)?)?;
    let m = pda::pubkey_bytes(mint)?;
    let wsol = pda::pubkey_bytes(WSOL_MINT)?;
    pda::derive(&[b"pool", &0u16.to_le_bytes(), &authority, &m, &wsol], PUMPSWAP)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    pub index: u16,
    pub creator: String,
    pub base_mint: String,
    pub quote_mint: String,
    pub lp_mint: String,
    pub pool_base_token_account: String,
    pub pool_quote_token_account: String,
    pub lp_supply: u64,
    pub coin_creator: Option<String>,
}

pub fn parse_pool(data: &[u8]) -> Option<Pool> {
    if data.len() < 8 || data[..8] != discriminator("account", "Pool") {
        return None;
    }
    let mut r = Reader::new(&data[8..]);
    r.u8()?; // bump
    let index = u16::from_le_bytes([r.u8()?, r.u8()?]);
    let creator = r.pubkey()?;
    let base_mint = r.pubkey()?;
    let quote_mint = r.pubkey()?;
    let lp_mint = r.pubkey()?;
    let pool_base_token_account = r.pubkey()?;
    let pool_quote_token_account = r.pubkey()?;
    let lp_supply = r.u64()?;
    let coin_creator = if r.remaining() >= 32 { r.pubkey() } else { None };
    Some(Pool {
        index,
        creator,
        base_mint,
        quote_mint,
        lp_mint,
        pool_base_token_account,
        pool_quote_token_account,
        lp_supply,
        coin_creator,
    })
}

/// `amount` of an SPL / Token-2022 token account (both put it at offset 64).
pub fn token_account_amount(data: &[u8]) -> Option<u64> {
    data.get(64..72).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// `supply` (offset 36) and `decimals` (offset 44) of a mint account.
pub fn mint_supply_and_decimals(data: &[u8]) -> Option<(u64, u8)> {
    let supply = u64::from_le_bytes(data.get(36..44)?.try_into().ok()?);
    let decimals = *data.get(44)?;
    Some((supply, decimals))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_pda_matches_fixture_account() {
        // pumpfun_sell_legacy_outer: mint CKeb…pump, bonding_curve = accounts[3]
        assert_eq!(
            bonding_curve_pda("CKebqzmi4fSEgLtnWqSFoM52qS2bf958EXkCpNSDpump").unwrap(),
            "Hp5gZQUiGnyRgH9TqCj6kjVHS56MaGGqyoeV2peehsUq"
        );
    }

    #[test]
    fn canonical_pool_pda_matches_fixture_pool() {
        // pumpswap_buy_exact_quote_in: mint 2syw…pump traded on pool 8JUa…
        assert_eq!(
            canonical_pool_pda("2sywcwJdrYWXr7h3xqNAoSKQfyutcBnFeVg5vP96pump").unwrap(),
            "8JUaR6h1xbD3RHqwWUGDfVRCTQe98W4CsypvXAuhRhfd"
        );
    }

    #[test]
    fn progress_and_mcap_arithmetic() {
        let c = BondingCurve {
            virtual_token_reserves: 1_000_000_000_000_000, // 1e9 tokens
            virtual_sol_reserves: 30_000_000_000,          // 30 SOL
            real_token_reserves: CURVE_INITIAL_REAL_TOKENS / 2,
            real_sol_reserves: 0,
            token_total_supply: 1_000_000_000_000_000,
            complete: false,
            creator: None,
        };
        assert_eq!(c.progress_bp(), 5_000);
        assert_eq!(c.mcap_lamports(), 30_000_000_000); // price × supply == vsol when vtok == supply
        let done = BondingCurve { complete: true, ..c };
        assert_eq!(done.progress_bp(), 10_000);
    }
}
