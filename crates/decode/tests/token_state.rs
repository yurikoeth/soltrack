//! Bonding-curve / PumpSwap-pool account parsing against live accounts
//! captured 2026-09-09 (`tests/fixtures/token_state.json`).

use base64::Engine;
use decode::curve::{
    bonding_curve_pda, canonical_pool_pda, parse_bonding_curve, parse_pool, token_account_amount,
};
use serde_json::Value;

fn fixtures() -> Value {
    let path = format!("{}/tests/fixtures/token_state.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn data(acct: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(acct["data_base64"].as_str().unwrap())
        .unwrap()
}

#[test]
fn completed_curve_parses_and_pda_matches() {
    let f = fixtures();
    let c = &f["curve_complete"];
    let mint = c["mint"].as_str().unwrap();
    assert_eq!(bonding_curve_pda(mint).unwrap(), c["curve"].as_str().unwrap());
    assert_eq!(c["account"]["owner"], decode::pumpfun::PROGRAM_ID);
    let curve = parse_bonding_curve(&data(&c["account"])).expect("BondingCurve account");
    assert!(curve.complete, "DCAT migrated to PumpSwap, so its curve is complete");
    assert_eq!(curve.progress_bp(), 10_000);
    assert_eq!(curve.token_total_supply, 1_000_000_000_000_000); // 1B × 10^6
    assert!(curve.creator.is_some());
}

#[test]
fn pool_parses_and_reserves_give_a_price() {
    let f = fixtures();
    let p = &f["pool"];
    let mint = p["mint"].as_str().unwrap();
    assert_eq!(canonical_pool_pda(mint).unwrap(), p["pool"].as_str().unwrap());
    assert_eq!(p["account"]["owner"], decode::pumpswap::PROGRAM_ID);
    let pool = parse_pool(&data(&p["account"])).expect("Pool account");
    assert_eq!(pool.index, 0);
    assert_eq!(pool.base_mint, mint);
    assert_eq!(pool.quote_mint, decode::WSOL_MINT);
    assert_eq!(pool.pool_base_token_account, p["base_token_account"]["pubkey"].as_str().unwrap());
    assert_eq!(pool.pool_quote_token_account, p["quote_token_account"]["pubkey"].as_str().unwrap());
    assert!(pool.coin_creator.is_some());
    let base = token_account_amount(&data(&p["base_token_account"])).unwrap();
    let quote = token_account_amount(&data(&p["quote_token_account"])).unwrap();
    assert!(base > 0 && quote > 0);
    // 1B supply pump token: price × supply should be a sane market cap (1 SOL .. 1M SOL)
    let mcap = quote as u128 * 1_000_000_000_000_000u128 / base as u128;
    assert!(mcap > 1_000_000_000 && mcap < 1_000_000_000_000_000, "mcap {mcap} lamports");
}

#[test]
fn live_curve_if_captured() {
    let f = fixtures();
    let Some(c) = f.get("curve_live").filter(|c| !c["account"].is_null()) else {
        eprintln!("no live curve fixture captured; skipping");
        return;
    };
    let curve = parse_bonding_curve(&data(&c["account"])).unwrap();
    assert!(!curve.complete);
    assert!(curve.progress_bp() < 10_000);
    assert!(curve.price().is_some());
    assert!(curve.mcap_lamports() > 0);
    assert_eq!(bonding_curve_pda(c["mint"].as_str().unwrap()).unwrap(), c["curve"].as_str().unwrap());
}

#[test]
fn create_v2_transaction_yields_a_launch_with_dev_buy() {
    let path = format!("{}/tests/fixtures/pumpfun_create.json", env!("CARGO_MANIFEST_DIR"));
    let tx: decode::RawTransaction = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let launch = decode::pumpfun::find_launch(&tx).expect("CreateEvent in tx");
    assert_eq!(launch.create.name, "SOUTHAMERICA");
    assert_eq!(launch.create.symbol, "SA");
    assert_eq!(launch.create.mint, "GRVdAjd4RC2gPDjHYwsKpqRPseet8HVTZbogjQBQpump");
    assert_eq!(launch.create.bonding_curve, "DeJRZqJsNnvwe4yqSvzzQcpKWpxDKM8PZExWdPbMMEcQ");
    assert_eq!(launch.create.user, "5qFTCR5tsC8nTieZ6XwMz1uEJytRADt2SQhtqWLfRXSM");
    assert_eq!(launch.create.creator.as_deref(), Some("5qFTCR5tsC8nTieZ6XwMz1uEJytRADt2SQhtqWLfRXSM"));
    assert_eq!(launch.create.timestamp, 1_788_962_601);
    assert_eq!(launch.create.token_total_supply, 1_000_000_000_000_000);
    assert_eq!(launch.create.virtual_sol_reserves, 30_000_000_000);
    assert_eq!(launch.create.real_token_reserves, decode::curve::CURVE_INITIAL_REAL_TOKENS);
    // initial mcap = 30 SOL x 1e9 supply / 1.073e9 virtual tokens ~ 27.96 SOL
    assert_eq!(launch.create.mcap_lamports(), 30_000_000_000u128 * 1_000_000_000_000_000 / 1_073_000_000_000_000);
    // dev bought in the same tx: curve leg 9_531_291 + fee 90_548 + creator fee 28_594
    assert_eq!(launch.dev_buy_lamports, 9_531_291 + 90_548 + 28_594);
    assert_eq!(launch.dev_buy_tokens, 340_794_202_085);
    assert_eq!(bonding_curve_pda(&launch.create.mint).unwrap(), launch.create.bonding_curve);
}

#[test]
fn garbage_is_rejected() {
    assert_eq!(parse_bonding_curve(&[0u8; 100]), None);
    assert_eq!(parse_pool(&[0u8; 300]), None);
    assert_eq!(token_account_amount(&[0u8; 10]), None);
}
