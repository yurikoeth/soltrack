# soltrack

Desktop overlay that follows a list of Solana wallets, decodes their Pump.fun /
PumpSwap memecoin trades in realtime, and shows running SOL-denominated PnL per
wallet. Read-only: no signing, no keys.

Rust + Tauri 2, React/TS/Tailwind webview, SQLite.

```
crates/
  decode/   raw getTransaction JSON  →  SwapEvent | UnknownSwap | NotASwap
  store/    SQLite behind `Storage`; idempotent on (signature, ix_index)
  pnl/      average cost basis, realized/unrealized, integer math only
  ingest/   WS logsSubscribe → getTransaction, backfill, rate limiter
src-tauri/  config, pipeline (ingest → decode → store → pnl → events), window
src/        overlay UI
```

## Run

```sh
npm install
cp soltrack.example.toml soltrack.toml   # add wallet addresses
npm run tauri dev
```

Wallets can be plain strings or `{ address = "...", label = "whale" }` — the
label replaces the shortened address everywhere in the UI.

Config lookup order: `$SOLTRACK_CONFIG`, `./soltrack.toml`, `../soltrack.toml`,
then `%APPDATA%\dev.mangin.soltrack\soltrack.toml` (a template is written
there on first run if nothing is found). The database lives in
`%APPDATA%\dev.mangin.soltrack\soltrack.db`.

The public RPC endpoint works but throttles hard (`requests_per_second = 4`
is about what it tolerates). Any provider key makes the backfill much faster —
e.g. Helius: `rpc_url = "https://mainnet.helius-rpc.com/?api-key=…"`,
`ws_url = "wss://mainnet.helius-rpc.com/?api-key=…"`, then raise
`requests_per_second` to 20–50.

## Release build

```sh
npm run tauri build   # target/release/bundle/{nsis,msi}/soltrack_<version>_x64-*
```

The installed app uses the same config lookup order as above (the working
directory is normally the install folder, so the per-user config dir is where
the template lands); a broken config opens the app with a banner naming the
parse error instead of exiting.

## Test

```sh
cargo test            # decode/store/pnl/ingest (default-members)
npm run build && cargo test --workspace   # + src-tauri (needs dist/)
```

`crates/decode/tests/fixtures/` holds real mainnet transactions (captured
2026-09-08); the expected amounts in `tests/fixtures.rs` were reconciled by
hand against each fixture's pre/post balances.

## How amounts are derived

Three decoder families, most specific first. A router/DEX instruction that
decodes suppresses decoding of the instructions nested under it.

**Pump.fun / PumpSwap** — both emit an Anchor `emit_cpi!` event as a self-CPI
one level below the swap instruction. That event, not the instruction args
(which only carry slippage limits) nor the wallet's lamport delta (polluted by
tx fees, rent, tips), is the source of truth:

- `TradeEvent` — `sol_amount` is the bonding-curve leg; `fee` and
  `creator_fee` are separate. Net paid on a buy = `sol_amount + fees`, net
  received on a sell = `sol_amount − fees`. Any Pump.fun instruction that emits
  a `TradeEvent` is decoded (`buy`, `sell`, `buy_v2`, `sell_v2`, …).
- `BuyEvent` / `SellEvent` — buy cost =
  `quote_amount_in_with_lp_fee + protocol_fee + coin_creator_fee`; sell
  proceeds = `user_quote_amount_out`. Mints come from the parent instruction's
  accounts 3/4. Inverted pools (`base = WSOL`) are handled.

The trader is the event's `user`, so bot-/router-routed trades attribute to
the actual wallet, not the fee payer.

**Jupiter v6** — every route leg emits a `SwapEvent` (amm, input/output mint
and amount). Legs are netted per mint; the result must be exactly
`{WSOL, one token}`. The trader is the signer who authorised the route's
token transfers (pool PDAs never sign).

**Raydium AMM v4 / CPMM / CLMM, Meteora DLMM / DAMM v2, Orca Whirlpool** — no
usable event, so the SPL `Transfer`/`TransferChecked` instructions under the
swap are reconciled to the tracked wallet's accounts: exactly one leg out of a
wallet-owned account, one leg into one, one of them WSOL. A temporary WSOL
account created and closed within the tx is invisible in balance metadata;
the only inference made is that such an account opposite a resolved token leg
is the SOL leg.

**Corroboration** — after decoding, each (wallet, mint) with swaps on one side
only must agree with the wallet's net balance change in that mint. If it
doesn't, the mint was an intermediate hop through a venue we don't decode (or
a router pocketed the proceeds), and the whole tx becomes *Unknown* rather
than a phantom position. Arbitrage chains and LP operations end up there too.

Token decimals come from the node's token-balance metadata for the mint,
never hardcoded.

If a tracked wallet's non-SOL token balance changes and nothing decodes, the
transaction is stored as an `UnknownSwap` (with the top-level programs it
invoked) and shown on the *unknown* tab. It never affects PnL.

## Token symbols

Mints are resolved to symbol/name lazily, the first time they're seen, and
cached in the `token_meta` table: Token-2022 mints (every Pump.fun mint since
2025) carry a `TokenMetadata` extension on the mint account itself; legacy
SPL mints fall back to the Metaplex metadata PDA. Parsing is fixture-tested
(`crates/ingest/tests/metadata.rs`). Hover any symbol for the full mint;
click it to open Solscan.

## UI

- **AA** button cycles text size (compact / normal / large; remembered).
- Click a wallet row to expand its positions: held, cost, mark-to-last-trade
  value, unrealized (with %), realized — open positions first, closed ones
  dimmed below.
- Hover a feed row for copy-signature / open-in-Solscan; the *unknown* tab
  names the programs involved (Jupiter, Raydium, Meteora, …).
- Pin button toggles always-on-top.

## Stats & alerts (the screener part)

Every wallet carries `WalletStats` (in `pnl`): token-level **win rate**
(exited tokens with realized > 0 vs < 0), **median hold** (first buy → first
sell), **median / max buy size**, volume, and **realized over 24h / 7d /
30d**. The win column is in the wallet table; the rest is in the expanded row.

Alerts fire on *live* swaps only — backfilled history never alerts:

```toml
[alerts]
enabled = true
min_buy_sol = 0.25        # ignore buys smaller than this
big_buy_multiple = 3.0    # "big buy" = >= N x the wallet's median buy (needs 5+ buys of history)
on_sell = false
notify = true             # OS toast
sound = true              # in-app beep (two-tone for big buys)
```

An alert = in-app toast + *alerts* tab entry + beep + OS notification. The
bell button mutes sound and toasts without changing config. The "big buy"
signal is the one worth watching: a wallet sizing up far beyond its norm is
conviction, a plain buy is noise.

## Discover (who bought first?)

The *discover* tab takes a mint address, walks that mint's signature history
back to launch (up to `discover.max_pages` × 1000 signatures, one RPC call per
page), decodes the oldest `discover.window` transactions with every wallet
treated as tracked, and lists the buyers in order. If the page budget runs out
before the first transaction, the result says so — you are then looking at the
oldest slice of what was scanned, not the launch; raise `max_pages` or use a
faster RPC. Each buyer is listed — rank, seconds after launch, size, whether they added or
already sold inside the window. **track** appends the wallet to
`soltrack.toml` (comments preserved), the ingest loop resubscribes live, and
backfill pulls its history so the stats fill in within a minute. Untrack from
the expanded wallet row.

Same thing from a terminal:

```sh
cargo run -p ingest --example discover -- <MINT> [--rpc URL] [--window 60] [--pages 30]
```

## Tokens, watchlist, launches

**tokens** — every mint your tracked wallets touched in the last
`tokens.window_hours`, ranked by how many *distinct* wallets bought it and the
net SOL they put in, with holders, their PnL on it, and the token's on-chain
state (bonding-curve progress or "migrated", market cap in SOL). A
**confluence** alert fires when `confluence_wallets` of your wallets buy the
same mint within `confluence_minutes` (one alert per mint per window) — the
one smart-money signal that is actually predictive, and only as good as the
wallets you have filtered.

**watch** — pin any mint (from tokens / launches / discover / paste). The
poller reads its bonding-curve account (or, once migrated, the canonical
PumpSwap pool's vaults) every `watch.poll_secs`, keeps a sparkline of market
cap, and alerts on migration, on curve progress passing
`alert_progress_pct`, and on market cap crossing `alert_mcap_sol` upward.
All on-chain reads — no price API.

**launches** — a second `logsSubscribe`, on the Pump.fun program itself. The
notification carries the log lines, so only transactions logging
`Instruction: Create`/`CreateV2` are fetched — still up to one a second at
busy times, so launches use their own best-effort budget
(`launches.requests_per_second`, default 1) and are *dropped* when over it,
never queued ahead of wallet traffic. Each row: symbol/name
(from the `CreateEvent`, so no metadata lookup), the dev's buy in the launch
transaction, launch market cap, and the creator — with one-click **watch**
and **track dev**. Scoring launches beyond that (holder distribution, dev
history) is deliberately not attempted here.

Every token shows its pump.fun image: the on-chain metadata `uri` points at
a JSON whose `image` field is the PNG (IPFS gateways; `ipfs://` is
normalised). The resolver records the URL (`token_meta.image`, schema v4),
then downloads the bytes once — trying ipfs.io, Cloudflare, Pinata and
dweb.link in turn, since ipfs.io refuses hotlinks from the webview — into
`%APPDATA%\dev.mangin.soltrack\images\<mint>.<ext>`, served to the UI via
Tauri's asset protocol. Remote URL and a tinted initial are the fallbacks. Token rows carry a chart link (`[ui] chart_url`, default
DexScreener — set it to GMGN/Photon/whatever you use) and a pump.fun page link.

## Paper trading (`[sim]`)

Every wallet row carries what copying it would have returned under the
configured rule — default: fill 15 s after the target at a 2% worse price,
0.1 SOL per copied buy, mirror the target's sells proportionally (also
delayed). Prices are only known at the target's own trades, so:

- a target sell that lands *inside* the delay can't be mirrored — we hold the
  bag (`missed exits`);
- `adverse entries` counts fills where the next observed price was already
  below our entry;
- optional `stop_loss_pct` / `take_profit_pct` exit at the next observed price.

`sim` is a pure crate (`crates/sim`, integer math, hand-verified tests); the
app recomputes it per wallet whenever that wallet trades. Treat a negative
"copy" number as disqualifying and a positive one as *necessary, not
sufficient* — it ignores liquidity and MEV.

## PnL

Per wallet per mint, average cost: buys add `sol_amount` to cost and
`token_amount` to quantity; a sell removes `cost × sold / qty` of basis and
realizes `proceeds − removed`. Unrealized = `qty × last_price − cost`, with
last price = last observed trade for that mint. Sells exceeding the tracked
quantity are treated as zero-basis and flagged (`oversold_events`). All math
in `u128`/`i128`; products before divisions.

## Not yet

USD pricing, geyser, trading. The launch feed is a firehose subscription;
the public WebSocket copes, but a provider key makes everything on this page
faster and calmer.
The `ingest::Transport` trait is where a Yellowstone gRPC source would plug in.
