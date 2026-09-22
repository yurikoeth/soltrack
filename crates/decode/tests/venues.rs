//! Jupiter / Raydium / Meteora / Orca fixtures captured 2026-09-09 (slots
//! 445490932–445491234). Expected amounts reconciled against each fixture's
//! pre/post balances; the negative cases are arbitrage bots and LP
//! operations that must NOT become positions.

use decode::{decode_transaction, DecodedTx, RawTransaction, Side, Venue};

const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

fn load(name: &str) -> RawTransaction {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn payer(tx: &RawTransaction) -> String {
    tx.transaction.message.account_keys[0].clone()
}

fn one_swap(tx: &RawTransaction, wallet: &str) -> decode::SwapEvent {
    match decode_transaction(tx, &[wallet.to_string()]) {
        DecodedTx::Swaps(mut v) => {
            assert_eq!(v.len(), 1, "expected exactly one swap, got {v:#?}");
            v.pop().unwrap()
        }
        other => panic!("expected Swaps, got {other:#?}"),
    }
}

fn expect_unknown(name: &str) -> decode::UnknownSwap {
    let tx = load(name);
    match decode_transaction(&tx, &[payer(&tx)]) {
        DecodedTx::Unknown(u) => u,
        other => panic!("{name}: expected Unknown, got {other:#?}"),
    }
}

// ---------------------------------------------------------------- Jupiter

#[test]
fn jupiter_sell_usdc_for_sol() {
    let tx = load("jupiter_v6_0");
    let ev = one_swap(&tx, &payer(&tx));
    assert_eq!(ev.venue, Venue::Jupiter);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, USDC);
    assert_eq!(ev.token_amount, 199_943_887); // == wallet USDC delta
    assert_eq!(ev.token_decimals, 6);
    assert_eq!(ev.sol_amount, 1_926_958_996); // == wallet WSOL delta
}

#[test]
fn jupiter_buy_usdc_with_sol() {
    let tx = load("jupiter_v6_1");
    let ev = one_swap(&tx, &payer(&tx));
    assert_eq!(ev.venue, Venue::Jupiter);
    assert_eq!(ev.side, Side::Buy);
    assert_eq!(ev.mint, USDC);
    assert_eq!(ev.token_amount, 199_943_887);
    assert_eq!(ev.sol_amount, 1_926_800_000);
}

#[test]
fn jupiter_arbitrage_leg_is_not_booked() {
    // payer routes SOL→USDC via a bot program, then USDC→USDT→SOL via
    // Jupiter; net USDC delta is 0, so the Jupiter leg alone must not
    // become a "sell USDC" position.
    let u = expect_unknown("jupiter_routed_unknown");
    assert!(u.programs.iter().any(|p| p == Venue::Jupiter.program_id()));
}

// ---------------------------------------------------------------- Meteora

#[test]
fn meteora_damm_v2_direct_buy() {
    let tx = load("meteora_damm_v2_1");
    let ev = one_swap(&tx, &payer(&tx));
    assert_eq!(ev.venue, Venue::MeteoraDammV2);
    assert_eq!(ev.side, Side::Buy);
    assert!(ev.mint.starts_with("vJ2dRt"), "{}", ev.mint);
    assert_eq!(ev.token_amount, 297_972_291); // == wallet token delta
    assert_eq!(ev.sol_amount, 100_000); // == wallet WSOL delta
    assert_eq!(ev.token_decimals, 6);
}

#[test]
fn meteora_damm_v2_sell_with_proceeds_routed_elsewhere_is_unknown() {
    // Two DAMM swaps under a router; the WSOL leaves the pool into the
    // router's account, not the wallet's, so the wallet's proceeds are not
    // observable from the swap instruction → Unknown, never a guess.
    let u = expect_unknown("meteora_damm_v2_0");
    assert_eq!(u.wallet, "6jLBJBpFZvevRsEMP7FPWbKLtCkJKY1dvJk1vBUZ8FVK");
}

#[test]
fn meteora_dlmm_leg_of_multihop_is_unknown() {
    // DLMM buys Pren1…, then an undecoded venue swaps Pren1… → CJnbkf…;
    // the wallet's Pren1 delta is 0 → uncorroborated → Unknown.
    expect_unknown("meteora_dlmm_0");
}

#[test]
fn meteora_dlmm_liquidity_withdrawal_is_unknown() {
    // pool → user token transfer with nothing going the other way: an LP
    // operation, not a swap.
    expect_unknown("meteora_dlmm_1");
}

// ---------------------------------------------------------------- Raydium (transfer-reconciled)

#[test]
fn raydium_amm_v4_sell_into_temp_wsol_account() {
    // The WSOL destination is created and closed inside the tx, so it has
    // no balance metadata; the decoder infers it is the SOL leg because the
    // outgoing leg is a resolved token.
    let tx = load("raydium_amm_v4_direct_0");
    let ev = one_swap(&tx, &payer(&tx));
    assert_eq!(ev.venue, Venue::RaydiumAmmV4);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB"); // USDT
    assert_eq!(ev.token_amount, 2_982_000); // swap leg (a further 18_000 left the wallet as a bot fee)
    assert_eq!(ev.sol_amount, 28_629_917);
    assert_eq!(ev.token_decimals, 6);
}

#[test]
fn raydium_amm_v4_sell_usdc() {
    let tx = load("raydium_amm_v4_direct_1");
    let ev = one_swap(&tx, &payer(&tx));
    assert_eq!(ev.venue, Venue::RaydiumAmmV4);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, USDC);
    assert_eq!(ev.token_amount, 3_256_174); // == wallet USDC delta
    assert_eq!(ev.sol_amount, 31_256_502);
}

#[test]
fn raydium_clmm_sell_pump_token_outer_instruction() {
    let tx = load("raydium_clmm_direct_0");
    let ev = one_swap(&tx, &payer(&tx));
    assert_eq!(ev.venue, Venue::RaydiumClmm);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn");
    assert_eq!(ev.token_amount, 26_884_867_423); // == wallet token delta
    assert_eq!(ev.sol_amount, 1_143_628_984); // == wallet WSOL delta
    assert_eq!(ev.token_decimals, 6);
}

#[test]
fn raydium_clmm_sell_usdc_transfer_checked() {
    let tx = load("raydium_clmm_direct_1");
    let ev = one_swap(&tx, &payer(&tx));
    assert_eq!(ev.venue, Venue::RaydiumClmm);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, USDC);
    assert_eq!(ev.token_amount, 4_352_454);
    assert_eq!(ev.sol_amount, 41_887_858);
}

#[test]
fn dex_swap_by_untracked_signer_is_not_attributed_to_the_pool() {
    // Tracking the pool authority must not turn the pool's side of the
    // swap into a trade: only signers can be traders.
    let tx = load("raydium_clmm_direct_0");
    let pool_authority = "45ssPjLkH2AFfgazvbaUAZmAgZ1xLbVAGVanLaQdfp1K".to_string();
    assert!(!tx.is_signer(&pool_authority));
    assert!(!matches!(decode_transaction(&tx, &[pool_authority]), DecodedTx::Swaps(_)));
}

// ---------------------------------------------------------------- arbitrage chains

#[test]
fn arbitrage_chains_through_multiple_venues_are_unknown() {
    for name in [
        "orca_whirlpool_0",
        "orca_whirlpool_direct_0", // Orca leg is token→token inside a 3-hop arb
        "raydium_amm_v4_0",
        "raydium_amm_v4_1",
        "raydium_clmm_0",
        "raydium_clmm_1",
        "raydium_cpmm_0",
        "raydium_cpmm_1",
    ] {
        let tx = load(name);
        match decode_transaction(&tx, &[payer(&tx)]) {
            DecodedTx::Unknown(_) => {}
            other => panic!("{name}: expected Unknown, got {other:#?}"),
        }
    }
}

#[test]
fn venue_ids_round_trip() {
    for v in Venue::ALL {
        assert_eq!(Venue::parse(v.as_str()), Some(v));
        assert_eq!(Venue::from_program(v.program_id()), Some(v));
    }
}
