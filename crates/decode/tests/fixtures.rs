//! Decode tests against real mainnet transactions captured 2026-09-08
//! (slots 445451264–445451265) via `getTransaction` with `encoding: json`.
//! Expected values were reconciled by hand against the pre/post lamport and
//! token balances in each fixture (see the module docs in `pumpfun.rs` and
//! `pumpswap.rs` for the derivations).

use decode::{decode_transaction, DecodedTx, RawTransaction, Side, SwapEvent, Venue};

fn load(name: &str) -> RawTransaction {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn one_swap(tx: &RawTransaction, wallet: &str) -> SwapEvent {
    match decode_transaction(tx, &[wallet.to_string()]) {
        DecodedTx::Swaps(mut v) => {
            assert_eq!(v.len(), 1, "expected exactly one swap, got {v:#?}");
            v.pop().unwrap()
        }
        other => panic!("expected Swaps, got {other:#?}"),
    }
}

// ---------------------------------------------------------------- Pump.fun

#[test]
fn pumpfun_legacy_sell_outer_instruction() {
    let tx = load("pumpfun_sell_legacy_outer");
    let wallet = "CwqvuoeFM4MrYRQKrxqni5K2JwuhKwzmTT88Q1tHX8Rg";
    let ev = one_swap(&tx, wallet);
    assert_eq!(ev.signature, tx.signature());
    assert_eq!(ev.slot, 445451264);
    assert_eq!(ev.block_time, Some(1788906323));
    assert_eq!(ev.wallet, wallet);
    assert_eq!(ev.mint, "CKebqzmi4fSEgLtnWqSFoM52qS2bf958EXkCpNSDpump");
    assert_eq!(ev.venue, Venue::PumpFunCurve);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.token_amount, 237_419_015_414);
    assert_eq!(ev.token_decimals, 6);
    // curve paid 66_072_319; protocol fee 627_688; creator fee 198_217
    assert_eq!(ev.fee_lamports, 627_688 + 198_217);
    assert_eq!(ev.sol_amount, 66_072_319 - 627_688 - 198_217);
    // wallet lamport delta (+64_152_614) == net proceeds - tx fee (1_093_800)
    assert_eq!(ev.sol_amount - tx.meta.fee, 64_152_614);
}

#[test]
fn pumpfun_legacy_buy_via_router_trader_is_not_fee_payer() {
    let tx = load("pumpfun_buy_legacy_via_router");
    // fee payer is Gygj9QQb...; the trader (event.user / token owner) is BwWK...
    let trader = "BwWK17cbHxwWBKZkUYvzxLcNQ1YVyaFezduWbtm2de6s";
    assert_ne!(tx.transaction.message.account_keys[0], trader);
    let ev = one_swap(&tx, trader);
    assert_eq!(ev.venue, Venue::PumpFunCurve);
    assert_eq!(ev.side, Side::Buy);
    assert_eq!(ev.mint, "HyCTa7FKrsTYqLMVym7xBjexmtB7Pk6t8aiTXYcTpump");
    assert_eq!(ev.token_amount, 9_292_288_783_155);
    assert_eq!(ev.token_decimals, 6);
    assert_eq!(ev.fee_lamports, 0); // fee-exempt buy
    assert_eq!(ev.sol_amount, 19_804_682); // == trader lamport delta

    // Tracking only the fee payer must NOT attribute the swap to it.
    let payer = tx.transaction.message.account_keys[0].clone();
    assert_eq!(decode_transaction(&tx, &[payer]), DecodedTx::NotASwap);
}

#[test]
fn pumpfun_sell_v2_via_router() {
    let tx = load("pumpfun_sell_v2_via_router");
    let wallet = "6HhMApNvU5z7SK8Rtq2TEzEtN8ANfJG1mqmWbhHqRiAx";
    let ev = one_swap(&tx, wallet);
    assert_eq!(ev.venue, Venue::PumpFunCurve);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, "BitAst9t1moZhc88Ccwpk6a3gV6JmTXBEotiQmTZpump");
    assert_eq!(ev.token_amount, 785_810_398_108);
    assert_eq!(ev.fee_lamports, 2_068_142 + 653_098);
    assert_eq!(ev.sol_amount, 217_699_130 - 2_068_142 - 653_098);
}

#[test]
fn pumpfun_program_referenced_but_no_trade_is_not_a_swap() {
    let tx = load("pumpfun_program_touched_no_trade");
    let payer = tx.transaction.message.account_keys[0].clone();
    assert_eq!(decode_transaction(&tx, &[payer]), DecodedTx::NotASwap);
}

// ---------------------------------------------------------------- PumpSwap

#[test]
fn pumpswap_buy_exact_quote_in() {
    let tx = load("pumpswap_buy_exact_quote_in");
    let wallet = "A7E46NQ69YaK6yXh4UYJR5Jp4Lud6N4ycyXxiHaywSxv";
    let ev = one_swap(&tx, wallet);
    assert_eq!(ev.venue, Venue::PumpSwapAmm);
    assert_eq!(ev.side, Side::Buy);
    assert_eq!(ev.mint, "2sywcwJdrYWXr7h3xqNAoSKQfyutcBnFeVg5vP96pump");
    assert_eq!(ev.token_amount, 290_708_905_357);
    assert_eq!(ev.token_decimals, 6);
    // pool +543_858_130, protocol fee 271_387, creator fee 4_884_954
    assert_eq!(ev.sol_amount, 543_858_130 + 271_387 + 4_884_954);
    assert_eq!(ev.fee_lamports, 1_085_546 + 271_387 + 4_884_954);
}

#[test]
fn pumpswap_sell() {
    let tx = load("pumpswap_sell");
    let wallet = "j1RFEDhCVYXDKNwBK4TPhLi1ZQhwySeCmnxAHiNgatZ";
    let ev = one_swap(&tx, wallet);
    assert_eq!(ev.venue, Venue::PumpSwapAmm);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, "YDmQPiSKq8fkf7nD5ngHNrY3XQvAg8gx7fiNCmdpump");
    assert_eq!(ev.token_amount, 2_176_541);
    assert_eq!(ev.sol_amount, 343_776);
    // wallet lamport delta (+317_607) == proceeds - tx fee
    assert_eq!(ev.sol_amount - tx.meta.fee, 317_607);
    assert_eq!(ev.fee_lamports, 690 + 173 + 173);
}

#[test]
fn pumpswap_buy_small_amounts() {
    let tx = load("pumpswap_buy_small");
    let wallet = "9zMwAbwxYxXuEEHhrN2fzxfxgfQDXcTRT7RUxJzJXf5M";
    let ev = one_swap(&tx, wallet);
    assert_eq!(ev.side, Side::Buy);
    assert_eq!(ev.token_amount, 5_993);
    assert_eq!(ev.sol_amount, 15_606);
    // wallet lamport delta (-20_606) == -(paid + tx fee 5_000)
    assert_eq!(ev.sol_amount + tx.meta.fee, 20_606);
}

#[test]
fn pumpswap_inverted_pool_sell_token_via_buy_exact_quote_in() {
    // base_mint = WSOL, quote_mint = token: "buying base" is selling the token.
    let tx = load("pumpswap_inverted_pool_sell_via_buy_exact_quote_in");
    let wallet = "BUR32J9d5i7FnyAHqqQW3oURERGP5D1q1DRxqKTN6x9f";
    let ev = one_swap(&tx, wallet);
    assert_eq!(ev.venue, Venue::PumpSwapAmm);
    assert_eq!(ev.side, Side::Sell);
    assert_eq!(ev.mint, "CUZvHSzu71MvLBqwzTaZcEEf7FWs2SzirhmroNAneDNp");
    assert_eq!(ev.token_amount, 10_231_830_861_839); // == wallet token delta
    assert_eq!(ev.sol_amount, 2_200_236_931); // == wallet lamport delta + tx fee
    assert_eq!(ev.sol_amount - tx.meta.fee, 2_200_231_780);
    assert_eq!(ev.fee_lamports, 0); // fees taken in tokens
}

#[test]
fn pumpswap_inverted_pool_bundle_two_swaps_two_wallets() {
    let tx = load("pumpswap_inverted_pool_bundle");
    let buyer = "WTftV2yXPYXp2FMkQhF2fV5s9BXGuXyrnCky3qzMr3Y".to_string();
    let seller = "5Rrmxj2NfLgedcDjE1uVNP7G6UjeU8M3JVo3mre9nNTe".to_string();

    // Both tracked: both swaps come back, in instruction order, distinct ix_index.
    let DecodedTx::Swaps(swaps) = decode_transaction(&tx, &[buyer.clone(), seller.clone()]) else {
        panic!("expected swaps");
    };
    assert_eq!(swaps.len(), 2);
    assert!(swaps[0].ix_index < swaps[1].ix_index);
    assert_eq!(swaps[0].wallet, buyer);
    assert_eq!(swaps[0].side, Side::Buy); // SellEvent on inverted pool
    assert_eq!(swaps[0].sol_amount, 10_062_243_128); // == wallet lamport delta + tx fee
    assert_eq!(swaps[0].mint, "7a41M58C5EBjsvLvBpjXuymrMabTdpRSYVmiWSF4e4L5");
    assert_eq!(swaps[1].wallet, seller);
    assert_eq!(swaps[1].side, Side::Sell); // BuyEvent on inverted pool
    assert_eq!(swaps[1].sol_amount, 9_804_280_194); // == wallet lamport delta

    // Only one tracked: only that wallet's swap.
    let ev = one_swap(&tx, &seller);
    assert_eq!(ev.wallet, seller);
}

// ---------------------------------------------------------------- Unknown / not a swap

#[test]
fn jupiter_routed_swap_is_unknown_for_the_trader() {
    let tx = load("jupiter_routed_unknown");
    let keys = tx.account_keys();
    // the trader is whoever's non-WSOL token balance moved
    let trader = tx
        .token_deltas(&keys)
        .into_iter()
        .find(|d| d.mint != decode::WSOL_MINT && d.delta != 0 && !d.owner.is_empty())
        .map(|d| d.owner)
        .expect("fixture has a token delta");
    match decode_transaction(&tx, &[trader.clone()]) {
        DecodedTx::Unknown(u) => {
            assert_eq!(u.wallet, trader);
            assert_eq!(u.signature, tx.signature());
            assert!(u
                .programs
                .iter()
                .any(|p| p == "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4"));
        }
        other => panic!("expected Unknown, got {other:#?}"),
    }
}

#[test]
fn untracked_wallets_yield_not_a_swap() {
    for name in [
        "pumpfun_sell_legacy_outer",
        "pumpswap_sell",
        "jupiter_routed_unknown",
    ] {
        let tx = load(name);
        assert_eq!(
            decode_transaction(&tx, &["11111111111111111111111111111111".to_string()]),
            DecodedTx::NotASwap,
            "{name}"
        );
    }
}

#[test]
fn failed_transaction_is_not_a_swap() {
    let mut tx = load("pumpswap_sell");
    tx.meta.err = Some(serde_json::json!({"InstructionError": [3, {"Custom": 6001}]}));
    let wallet = "j1RFEDhCVYXDKNwBK4TPhLi1ZQhwySeCmnxAHiNgatZ".to_string();
    assert_eq!(decode_transaction(&tx, &[wallet]), DecodedTx::NotASwap);
}

#[test]
fn swap_event_round_trips_through_serde() {
    let tx = load("pumpswap_sell");
    let ev = one_swap(&tx, "j1RFEDhCVYXDKNwBK4TPhLi1ZQhwySeCmnxAHiNgatZ");
    let json = serde_json::to_string(&ev).unwrap();
    let back: SwapEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(ev, back);
}
