//! Who bought a token first?
//!
//! ```sh
//! cargo run -p ingest --example discover -- <MINT> [--rpc URL] [--window 60] [--pages 30]
//! ```

use std::sync::Arc;

use ingest::{DiscoverConfig, RateLimitConfig, RateLimiter, RpcClient};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let Some(mint) = args.next() else {
        eprintln!("usage: discover <MINT> [--rpc URL] [--window N] [--pages N]");
        std::process::exit(2);
    };
    let mut rpc_url = "https://api.mainnet-beta.solana.com".to_string();
    let mut cfg = DiscoverConfig::default();
    while let Some(flag) = args.next() {
        let val = args.next().unwrap_or_default();
        match flag.as_str() {
            "--rpc" => rpc_url = val,
            "--window" => cfg.window = val.parse().expect("--window N"),
            "--pages" => cfg.max_pages = val.parse().expect("--pages N"),
            _ => eprintln!("ignoring {flag}"),
        }
    }
    let rpc = RpcClient::new(rpc_url, Arc::new(RateLimiter::new(RateLimitConfig::default())));
    let started = std::time::Instant::now();
    let d = ingest::discover::early_buyers(&rpc, &mint, &cfg, |p| {
        eprint!("\r{:<9} {:>4}/{:<4}", p.phase, p.done, p.total);
    })
    .await
    .expect("discovery failed");
    eprintln!();
    let found = &d.candidates;
    println!(
        "{} early buyers of {mint} ({:.1}s, {} txs scanned, launch slot {})",
        found.len(),
        started.elapsed().as_secs_f32(),
        d.scanned,
        d.launch_slot
    );
    if !d.reached_launch {
        println!(
            "WARNING: page budget ({} x 1000) ran out before the first transaction - this is the oldest slice of what was scanned, not the launch. Raise --pages.",
            cfg.max_pages
        );
    }
    println!("{:>3}  {:<44} {:>8} {:>12} {:>14}  {:<13} {}", "#", "wallet", "+slots", "SOL", "tokens", "venue", "in window");
    for c in found {
        let sol = c.sol_amount.parse::<u64>().unwrap_or(0);
        let note = format!(
            "{}{}",
            if c.extra_buys > 0 { format!("+{} buys ", c.extra_buys) } else { String::new() },
            if c.sold_in_window { "sold" } else { "" }
        );
        println!(
            "{:>3}  {:<44} {:>8} {:>12.4} {:>14}  {:<13} {}",
            c.rank,
            c.wallet,
            c.slots_after_launch,
            sol as f64 / 1e9,
            c.token_amount,
            c.venue,
            note
        );
    }
}
