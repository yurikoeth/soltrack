//! Glue: `IngestEvent` → decode → store → pnl (+ sim) → webview events;
//! alerts on live swaps (incl. confluence); token-state refresher; watchlist
//! poller; launch feed; token metadata resolver.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use decode::{DecodedTx, Side, SwapEvent, TokenMeta};
use ingest::{ConnState, Cursor, IngestEvent, LaunchEvent, LaunchState, RpcClient, TokenState, WalletSet};
use pnl::tokens::ConfluenceRule;
use pnl::WalletPnl;
use sim::{SimConfig, SimResult};
use store::{Inserted, SqliteStorage, Storage};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::{Alerts, Launches, Tokens, Watch};
use crate::dto::{AlertRow, LaunchRow, MetaRow, SimRow, SwapRow, TokenRow, UnknownRow, WalletRow, WatchPoint, WatchRow};

pub type SharedStore = Arc<Mutex<SqliteStorage>>;

const ALERT_HISTORY: usize = 100;

pub struct WatchEntry {
    pub points: VecDeque<WatchPoint>,
    pub creator: Option<String>,
    pub alerted_progress: bool,
    pub alerted_migrated: bool,
    pub above_mcap: bool,
}

pub struct AppState {
    pub store: SharedStore,
    /// Live tracked set; shared with the ingest loop, which resubscribes on change.
    pub wallets: Arc<WalletSet>,
    pub labels: RwLock<HashMap<String, String>>,
    pub pnl: Mutex<HashMap<String, WalletPnl>>,
    pub sims: Mutex<HashMap<String, SimResult>>,
    pub sim_cfg: Option<SimConfig>,
    pub sim_describe: String,
    pub status: Mutex<ConnState>,
    pub fetch_failures: AtomicU64,
    pub config_path: std::path::PathBuf,
    pub config_created: bool,
    pub config_error: Option<String>,
    pub rpc_url: String,
    pub feed_limit: u32,
    /// Mints to look up display metadata for.
    pub meta_tx: mpsc::UnboundedSender<String>,
    /// Mints whose on-chain state should be (re)fetched.
    pub token_tx: mpsc::UnboundedSender<String>,
    pub alerts_cfg: Alerts,
    pub muted: AtomicBool,
    pub alerts: Mutex<VecDeque<AlertRow>>,
    pub alert_seq: AtomicU64,
    pub rpc: Arc<RpcClient>,
    pub discover_cfg: ingest::DiscoverConfig,
    pub tokens_cfg: Tokens,
    pub token_states: Mutex<HashMap<String, TokenState>>,
    /// last confluence alert per mint
    pub confluence_last: Mutex<HashMap<String, i64>>,
    pub watch_cfg: Watch,
    pub watch_mints: RwLock<Vec<String>>,
    pub watch: Mutex<BTreeMap<String, WatchEntry>>,
    pub launches_cfg: Launches,
    pub launches: Mutex<VecDeque<ingest::Launch>>,
    pub launch_state: Mutex<LaunchState>,
    pub chart_url: String,
    /// `<app data>/images` — downloaded token images, `<mint>.<ext>`
    pub image_dir: std::path::PathBuf,
}

impl AppState {
    pub fn confluence_rule(&self) -> ConfluenceRule {
        ConfluenceRule {
            min_wallets: self.tokens_cfg.confluence_wallets.max(1),
            window_secs: self.tokens_cfg.confluence_minutes as i64 * 60,
        }
    }

    /// Rebuild every tracked wallet's PnL (and sim) from stored history.
    /// Blocking; call off the async runtime.
    pub fn rebuild_all(&self, wallets: &[String]) -> anyhow::Result<()> {
        for w in wallets {
            self.rebuild_wallet(w)?;
        }
        Ok(())
    }

    pub fn rebuild_wallet(&self, wallet: &str) -> anyhow::Result<()> {
        let swaps = self.store.lock().unwrap_or_else(|e| e.into_inner()).swaps_for_wallet(wallet)?;
        self.pnl
            .lock()
            .unwrap()
            .insert(wallet.to_string(), WalletPnl::compute(wallet, &swaps));
        if let Some(cfg) = &self.sim_cfg {
            self.sims
                .lock()
                .unwrap()
                .insert(wallet.to_string(), sim::simulate(cfg, &swaps));
        }
        Ok(())
    }

    fn resim(&self, wallet: &str) -> anyhow::Result<()> {
        let Some(cfg) = &self.sim_cfg else { return Ok(()) };
        let swaps = self.store.lock().unwrap_or_else(|e| e.into_inner()).swaps_for_wallet(wallet)?;
        self.sims
            .lock()
            .unwrap()
            .insert(wallet.to_string(), sim::simulate(cfg, &swaps));
        Ok(())
    }

    pub fn forget_wallet(&self, wallet: &str) {
        self.pnl.lock().unwrap_or_else(|e| e.into_inner()).remove(wallet);
        self.sims.lock().unwrap_or_else(|e| e.into_inner()).remove(wallet);
        self.labels.write().unwrap_or_else(|e| e.into_inner()).remove(wallet);
    }

    pub fn label(&self, wallet: &str) -> Option<String> {
        self.labels.read().unwrap_or_else(|e| e.into_inner()).get(wallet).cloned()
    }

    pub fn wallet_row(&self, wallet: &str) -> WalletRow {
        let label = self.label(wallet);
        let sim = self
            .sims
            .lock()
            .unwrap()
            .get(wallet)
            .map(|r| SimRow::new(r, &self.sim_describe));
        let map = self.pnl.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(wallet) {
            Some(p) => WalletRow::new(p, label.as_deref(), sim, now()),
            None => WalletRow::new(&WalletPnl::new(wallet), label.as_deref(), sim, now()),
        }
    }

    pub fn wallet_rows(&self, wallets: &[String]) -> Vec<WalletRow> {
        wallets.iter().map(|w| self.wallet_row(w)).collect()
    }

    /// Local cache file for a mint's image, if downloaded.
    pub fn local_image(&self, mint: &str) -> Option<std::path::PathBuf> {
        local_image_in(&self.image_dir, mint)
    }

    pub fn meta_row(&self, m: &TokenMeta) -> MetaRow {
        MetaRow::new(m, self.local_image(&m.mint))
    }

    pub fn recent_alerts(&self) -> Vec<AlertRow> {
        self.alerts.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().cloned().collect()
    }

    pub fn is_watched(&self, mint: &str) -> bool {
        self.watch_mints.read().unwrap_or_else(|e| e.into_inner()).iter().any(|m| m == mint)
    }

    /// Tokens tab rows over the configured window. Blocking (store read).
    pub fn token_rows(&self) -> anyhow::Result<Vec<TokenRow>> {
        let since = now() - self.tokens_cfg.window_hours as i64 * 3600;
        let swaps = self.store.lock().unwrap_or_else(|e| e.into_inner()).swaps_since(since)?;
        let activity = pnl::tokens::token_activity(&swaps, since);
        let holdings = {
            let map = self.pnl.lock().unwrap_or_else(|e| e.into_inner());
            pnl::tokens::token_holdings(map.values())
        };
        let states = self.token_states.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<TokenRow> = activity
            .values()
            .map(|a| TokenRow::new(a, holdings.get(&a.mint), states.get(&a.mint), self.is_watched(&a.mint)))
            .collect();
        rows.sort_by(|x, y| {
            y.buyers
                .len()
                .cmp(&x.buyers.len())
                .then_with(|| {
                    let nx: i128 = x.net_flow_lamports.parse().unwrap_or(0);
                    let ny: i128 = y.net_flow_lamports.parse().unwrap_or(0);
                    ny.cmp(&nx)
                })
                .then_with(|| y.last_trade.cmp(&x.last_trade))
        });
        Ok(rows)
    }

    pub fn watch_rows(&self) -> Vec<WatchRow> {
        let mints = self.watch_mints.read().unwrap_or_else(|e| e.into_inner()).clone();
        let w = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        mints
            .iter()
            .map(|m| {
                let e = w.get(m);
                WatchRow {
                    mint: m.clone(),
                    points: e.map(|e| e.points.iter().cloned().collect()).unwrap_or_default(),
                    latest: e.and_then(|e| e.points.back().cloned()),
                    creator: e.and_then(|e| e.creator.clone()),
                }
            })
            .collect()
    }

    pub fn launch_rows(&self, tracked: &[String]) -> Vec<LaunchRow> {
        self.launches
            .lock()
            .unwrap()
            .iter()
            .rev()
            .map(|l| LaunchRow::new(l, tracked.contains(&l.creator), self.is_watched(&l.mint)))
            .collect()
    }

    fn push_alert(&self, app: &AppHandle, mut alert: AlertRow) -> AlertRow {
        alert.id = self.alert_seq.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut q = self.alerts.lock().unwrap_or_else(|e| e.into_inner());
            q.push_back(alert.clone());
            while q.len() > ALERT_HISTORY {
                q.pop_front();
            }
        }
        let _ = app.emit("alert", &alert);
        alert
    }

    fn notify(&self, app: &AppHandle, title: String, body: String) {
        if !self.alerts_cfg.notify || self.muted.load(Ordering::Relaxed) {
            return;
        }
        if let Err(e) = app.notification().builder().title(title).body(body).show() {
            tracing::warn!(error = %e, "notification failed");
        }
    }

    fn symbol_or_short(&self, mint: &str) -> String {
        self.store
            .lock()
            .unwrap()
            .token_meta(mint)
            .ok()
            .flatten()
            .filter(|m| !m.symbol.is_empty())
            .map(|m| m.symbol)
            .unwrap_or_else(|| short(mint))
    }
}

/// `Cursor` for ingest, backed by the shared store.
pub struct StoreCursor(pub SharedStore);

#[async_trait]
impl Cursor for StoreCursor {
    async fn has_signature(&self, signature: &str) -> bool {
        let store = self.0.clone();
        let sig = signature.to_string();
        tauri::async_runtime::spawn_blocking(move || {
            store.lock().unwrap_or_else(|e| e.into_inner()).has_signature(&sig).unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    }

    async fn last_seen_slot(&self, wallet: &str) -> Option<u64> {
        let store = self.0.clone();
        let w = wallet.to_string();
        tauri::async_runtime::spawn_blocking(move || {
            store.lock().unwrap_or_else(|e| e.into_inner()).last_seen_slot(&w).ok().flatten()
        })
        .await
        .ok()
        .flatten()
    }
}

pub async fn run(app: AppHandle, state: Arc<AppState>, mut rx: mpsc::Receiver<IngestEvent>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            IngestEvent::Status(s) => {
                tracing::info!(?s, "ingest status");
                *state.status.lock().unwrap_or_else(|e| e.into_inner()) = s.clone();
                let _ = app.emit("status", &s);
            }
            IngestEvent::FetchFailed { wallet, signature, error } => {
                tracing::warn!(%wallet, %signature, %error, "fetch failed");
                state.fetch_failures.fetch_add(1, Ordering::Relaxed);
            }
            IngestEvent::Transaction { wallet, tx, live } => {
                let tracked = state.wallets.get().await;
                let state = state.clone();
                let app = app.clone();
                // decode + sqlite are CPU/blocking work; keep them off the runtime
                let res = tauri::async_runtime::spawn_blocking(move || {
                    handle_transaction(&app, &state, &tracked, &wallet, &tx, live)
                })
                .await;
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::error!(error = %e, "pipeline error"),
                    Err(e) => tracing::error!(error = %e, "pipeline task panicked"),
                }
            }
        }
    }
    tracing::info!("pipeline: ingest channel closed");
}

fn handle_transaction(
    app: &AppHandle,
    state: &AppState,
    tracked: &[String],
    notified_wallet: &str,
    tx: &decode::RawTransaction,
    live: bool,
) -> anyhow::Result<()> {
    let decoded = decode::decode_transaction(tx, tracked);
    let sig = tx.signature();
    match decoded {
        DecodedTx::Swaps(swaps) => {
            let mut touched: HashSet<String> = HashSet::new();
            for ev in &swaps {
                let inserted = state.store.lock().unwrap_or_else(|e| e.into_inner()).insert_swap(ev)?;
                if inserted == Inserted::Duplicate {
                    tracing::debug!(sig, ix = ev.ix_index, "duplicate swap ignored");
                    continue;
                }
                tracing::info!(
                    sig, wallet = %ev.wallet, side = ev.side.as_str(), venue = ev.venue.as_str(),
                    mint = %ev.mint, tokens = ev.token_amount, lamports = ev.sol_amount, live, "swap"
                );
                apply_to_pnl(state, ev)?;
                touched.insert(ev.wallet.clone());
                let _ = state.meta_tx.send(ev.mint.clone());
                let _ = state.token_tx.send(ev.mint.clone());
                let _ = app.emit("swap", SwapRow::from(ev));
                if live {
                    maybe_alert(app, state, ev);
                    if ev.side == Side::Buy {
                        maybe_confluence(app, state, ev)?;
                    }
                }
            }
            for w in touched {
                state.resim(&w)?;
                let _ = app.emit("wallet", state.wallet_row(&w));
            }
            if !swaps.is_empty() {
                let _ = app.emit("tokens_changed", ());
            }
        }
        DecodedTx::Unknown(u) => {
            let inserted = state.store.lock().unwrap_or_else(|e| e.into_inner()).insert_unknown(&u)?;
            if inserted == Inserted::New {
                tracing::warn!(sig, wallet = %u.wallet, programs = ?u.programs, "UnknownSwap");
                let _ = app.emit("unknown", UnknownRow::from(&u));
            }
        }
        DecodedTx::NotASwap => {
            tracing::debug!(sig, "not a swap");
            state
                .store
                .lock()
                .unwrap()
                .mark_not_swap(sig, notified_wallet, tx.slot)?;
        }
    }
    Ok(())
}

/// Incremental update when the event is newer than everything seen; a full
/// replay from the store when it arrives out of order (late backfill etc.).
fn apply_to_pnl(state: &AppState, ev: &SwapEvent) -> anyhow::Result<()> {
    let mut map = state.pnl.lock().unwrap_or_else(|e| e.into_inner());
    let entry = map
        .entry(ev.wallet.clone())
        .or_insert_with(|| WalletPnl::new(&ev.wallet));
    let in_order = match &entry.last_trade {
        None => true,
        Some(last) => (ev.slot, ev.ix_index) >= (last.slot, last.ix_index),
    };
    if in_order {
        entry.apply(ev);
    } else {
        tracing::info!(wallet = %ev.wallet, "out-of-order swap; replaying wallet history");
        let swaps = state.store.lock().unwrap_or_else(|e| e.into_inner()).swaps_for_wallet(&ev.wallet)?;
        *entry = WalletPnl::compute(&ev.wallet, &swaps);
    }
    Ok(())
}

/// Minimum buys of history before "N× the median" means anything.
const BIG_BUY_MIN_HISTORY: usize = 5;

fn maybe_alert(app: &AppHandle, state: &AppState, ev: &SwapEvent) {
    let cfg = &state.alerts_cfg;
    if !cfg.enabled {
        return;
    }
    let is_buy = ev.side == Side::Buy;
    if !is_buy && !cfg.on_sell {
        return;
    }
    if is_buy && ev.sol_amount < cfg.min_buy_lamports() {
        return;
    }
    let (median, history) = {
        let map = state.pnl.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&ev.wallet)
            .map(|w| (w.median_buy_lamports(), w.buy_sizes.len()))
            .unwrap_or((None, 0))
    };
    let tenths = cfg.big_buy_multiple_tenths() as u128;
    let kind = if !is_buy {
        "sell"
    } else {
        match median {
            Some(m) if tenths > 0 && history >= BIG_BUY_MIN_HISTORY && m > 0
                && ev.sol_amount as u128 * 10 >= m as u128 * tenths =>
            {
                "big_buy"
            }
            _ => "buy",
        }
    };
    let label = state.label(&ev.wallet);
    tracing::info!(kind, wallet = %ev.wallet, lamports = ev.sol_amount, ?median, "alert");
    state.push_alert(
        app,
        AlertRow {
            id: 0,
            kind,
            at: now(),
            mint: ev.mint.clone(),
            wallet: Some(ev.wallet.clone()),
            label: label.clone(),
            swap: Some(SwapRow::from(ev)),
            median_buy_lamports: median.map(|m| m.to_string()),
            note: None,
        },
    );
    let symbol = state.symbol_or_short(&ev.mint);
    let who = label.unwrap_or_else(|| short(&ev.wallet));
    let title = match kind {
        "big_buy" => format!("{who} BIG BUY {} ◎ {symbol}", fmt_sol(ev.sol_amount)),
        "sell" => format!("{who} sold {symbol} for {} ◎", fmt_sol(ev.sol_amount)),
        _ => format!("{who} bought {symbol} · {} ◎", fmt_sol(ev.sol_amount)),
    };
    let body = match median {
        Some(m) if is_buy => format!("{} · median buy {} ◎", ev.venue.as_str(), fmt_sol(m)),
        _ => ev.venue.as_str().to_string(),
    };
    state.notify(app, title, body);
}

/// N distinct tracked wallets bought this mint inside the window → one
/// alert per mint per window.
fn maybe_confluence(app: &AppHandle, state: &AppState, ev: &SwapEvent) -> anyhow::Result<()> {
    let rule = state.confluence_rule();
    if rule.min_wallets < 2 {
        return Ok(());
    }
    let now = now();
    let recent = state.store.lock().unwrap_or_else(|e| e.into_inner()).swaps_since(now - rule.window_secs)?;
    let Some(wallets) = pnl::tokens::confluence(&recent, &ev.mint, rule, now) else {
        return Ok(());
    };
    {
        let mut last = state.confluence_last.lock().unwrap_or_else(|e| e.into_inner());
        if last.get(&ev.mint).map_or(false, |t| now - t < rule.window_secs) {
            return Ok(());
        }
        last.insert(ev.mint.clone(), now);
    }
    let names: Vec<String> = wallets
        .iter()
        .map(|w| state.label(w).unwrap_or_else(|| short(w)))
        .collect();
    let symbol = state.symbol_or_short(&ev.mint);
    let note = format!("{} wallets in {}m: {}", wallets.len(), rule.window_secs / 60, names.join(", "));
    tracing::info!(mint = %ev.mint, wallets = wallets.len(), "confluence alert");
    state.push_alert(
        app,
        AlertRow {
            id: 0,
            kind: "confluence",
            at: now,
            mint: ev.mint.clone(),
            wallet: Some(ev.wallet.clone()),
            label: state.label(&ev.wallet),
            swap: Some(SwapRow::from(ev)),
            median_buy_lamports: None,
            note: Some(note.clone()),
        },
    );
    state.notify(app, format!("CONFLUENCE {symbol}"), note);
    Ok(())
}

pub fn short(s: &str) -> String {
    if s.len() <= 9 {
        s.to_string()
    } else {
        format!("{}…{}", &s[..4], &s[s.len() - 4..])
    }
}

/// Lamports → "1.234" SOL for notification text. Integer formatting only.
fn fmt_sol(lamports: u64) -> String {
    let whole = lamports / 1_000_000_000;
    let milli = (lamports % 1_000_000_000) / 1_000_000;
    format!("{whole}.{milli:03}")
}

fn fmt_sol_u128(lamports: u128) -> String {
    let whole = lamports / 1_000_000_000;
    let deci = (lamports % 1_000_000_000) / 100_000_000;
    format!("{whole}.{deci}")
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn local_image_in(dir: &std::path::Path, mint: &str) -> Option<std::path::PathBuf> {
    for ext in ["png", "jpg", "gif", "webp", "svg"] {
        let p = dir.join(format!("{mint}.{ext}"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Resolves symbol/name for mints as they're first seen. Results are cached
/// in the store; a "no metadata on chain" answer is cached too (empty
/// symbol) so we don't hammer the RPC. Transient RPC errors are not cached —
/// the mint is retried the next time it trades.
pub async fn run_meta_resolver(
    app: AppHandle,
    store: SharedStore,
    rpc: Arc<RpcClient>,
    image_dir: std::path::PathBuf,
    mut rx: mpsc::UnboundedReceiver<String>,
) {
    let http = ingest::metadata::metadata_http_client();
    let _ = std::fs::create_dir_all(&image_dir);
    let emit_meta = |meta: &TokenMeta| {
        let local = local_image_in(&image_dir, &meta.mint);
        let _ = app.emit("meta", MetaRow::new(meta, local));
    };
    let mut done: HashSet<String> = HashSet::new();
    while let Some(mint) = rx.recv().await {
        if done.contains(&mint) {
            continue;
        }
        let cached = {
            let s = store.clone();
            let m = mint.clone();
            tauri::async_runtime::spawn_blocking(move || s.lock().unwrap_or_else(|e| e.into_inner()).token_meta(&m).ok().flatten())
                .await
                .ok()
                .flatten()
        };
        let mut meta = match cached {
            Some(m) => m,
            None => match ingest::metadata::fetch_token_meta(&rpc, &mint).await {
                Ok(found) => {
                    let meta = found.unwrap_or_else(|| TokenMeta {
                        mint: mint.clone(),
                        symbol: String::new(),
                        name: String::new(),
                        uri: String::new(),
                        image: String::new(),
                    });
                    tracing::info!(mint = %mint, symbol = %meta.symbol, "token metadata resolved");
                    let s = store.clone();
                    let m = meta.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || s.lock().unwrap_or_else(|e| e.into_inner()).put_token_meta(&m)).await;
                    if !meta.symbol.is_empty() || !meta.name.is_empty() {
                        emit_meta(&meta);
                    }
                    meta
                }
                Err(e) => {
                    tracing::warn!(mint = %mint, error = %e, "token metadata lookup failed");
                    continue;
                }
            },
        };
        done.insert(mint.clone());
        // second hop: the off-chain JSON at `uri` carries the image (pump.fun PNG)
        if meta.image.is_empty() && !meta.uri.is_empty() {
            if let Some(img) = ingest::metadata::fetch_image_url(&http, &meta.uri).await {
                tracing::debug!(mint = %mint, image = %img, "token image resolved");
                meta.image = img;
                let s = store.clone();
                let m = meta.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || s.lock().unwrap_or_else(|e| e.into_inner()).put_token_meta(&m)).await;
                emit_meta(&meta);
            }
        }
        // third hop: cache the bytes locally — the webview can't hotlink
        // ipfs.io reliably, and the asset protocol serves files instantly
        if !meta.image.is_empty() && local_image_in(&image_dir, &mint).is_none() {
            match ingest::metadata::download_image(&http, &meta.image).await {
                Some((bytes, ext)) => {
                    let path = image_dir.join(format!("{mint}.{ext}"));
                    match std::fs::write(&path, &bytes) {
                        Ok(()) => {
                            tracing::debug!(mint = %mint, bytes = bytes.len(), "token image cached");
                            emit_meta(&meta);
                        }
                        Err(e) => tracing::warn!(mint = %mint, error = %e, "image cache write failed"),
                    }
                }
                None => tracing::debug!(mint = %mint, image = %meta.image, "token image download failed"),
            }
        }
    }
}

/// Fetches on-chain token state for mints as they trade, at most once per
/// `tokens.refresh_secs` per mint.
pub async fn run_token_refresher(app: AppHandle, state: Arc<AppState>, mut rx: mpsc::UnboundedReceiver<String>) {
    let min_gap = state.tokens_cfg.refresh_secs as i64;
    while let Some(mint) = rx.recv().await {
        let fresh = state
            .token_states
            .lock()
            .unwrap()
            .get(&mint)
            .map_or(false, |s| now() - s.fetched_at < min_gap);
        if fresh {
            continue;
        }
        match ingest::tokens::fetch_token_state(&state.rpc, &mint).await {
            Ok(ts) => {
                let row = crate::dto::TokenStateRow::from(&ts);
                state.token_states.lock().unwrap_or_else(|e| e.into_inner()).insert(mint.clone(), ts);
                let _ = app.emit("token_state", (&mint, &row));
            }
            Err(e) => tracing::warn!(%mint, error = %e, "token state fetch failed"),
        }
    }
}

/// Polls every watched mint on a fixed cadence and raises watch alerts.
pub async fn run_watch_poller(app: AppHandle, state: Arc<AppState>, cancel: CancellationToken) {
    let period = Duration::from_secs(state.watch_cfg.poll_secs.max(5) as u64);
    let keep = state.watch_cfg.history_points.max(10) as usize;
    let progress_bp = (state.watch_cfg.alert_progress_pct.max(0.0) * 100.0).round() as u32;
    let mcap_threshold = (state.watch_cfg.alert_mcap_sol.max(0.0) * 1e9).round() as u128;
    loop {
        let mints = state.watch_mints.read().unwrap_or_else(|e| e.into_inner()).clone();
        for mint in mints {
            if cancel.is_cancelled() {
                return;
            }
            let ts = match ingest::tokens::fetch_token_state(&state.rpc, &mint).await {
                Ok(ts) => ts,
                Err(e) => {
                    tracing::warn!(%mint, error = %e, "watch poll failed");
                    continue;
                }
            };
            let point = WatchPoint::from(&ts);
            let mcap = ts.mcap_lamports();
            let mut fire: Vec<(&'static str, String)> = Vec::new();
            {
                let mut w = state.watch.lock().unwrap_or_else(|e| e.into_inner());
                let e = w.entry(mint.clone()).or_insert_with(|| WatchEntry {
                    points: VecDeque::new(),
                    creator: None,
                    alerted_progress: false,
                    alerted_migrated: false,
                    above_mcap: false,
                });
                if e.creator.is_none() {
                    e.creator = ts.curve.as_ref().and_then(|c| c.creator.clone());
                }
                let first = e.points.is_empty();
                if let Some(c) = &ts.curve {
                    if progress_bp > 0 && c.progress_bp >= progress_bp && !c.complete && !e.alerted_progress {
                        e.alerted_progress = true;
                        if !first {
                            fire.push(("progress", format!("curve {}% filled", c.progress_bp / 100)));
                        }
                    }
                    if c.complete && !e.alerted_migrated {
                        e.alerted_migrated = true;
                        if !first {
                            fire.push(("migrated", "graduated to PumpSwap".into()));
                        }
                    }
                }
                if mcap_threshold > 0 {
                    if let Some(m) = mcap {
                        let above = m >= mcap_threshold;
                        if above && !e.above_mcap && !first {
                            fire.push(("mcap", format!("mcap {} ◎", fmt_sol_u128(m))));
                        }
                        e.above_mcap = above;
                    }
                }
                e.points.push_back(point.clone());
                while e.points.len() > keep {
                    e.points.pop_front();
                }
            }
            state.token_states.lock().unwrap_or_else(|e| e.into_inner()).insert(mint.clone(), ts);
            let _ = app.emit("watch_point", (&mint, &point));
            for (kind, note) in fire {
                let symbol = state.symbol_or_short(&mint);
                tracing::info!(%mint, kind, %note, "watch alert");
                state.push_alert(
                    &app,
                    AlertRow {
                        id: 0,
                        kind,
                        at: now(),
                        mint: mint.clone(),
                        wallet: None,
                        label: None,
                        swap: None,
                        median_buy_lamports: None,
                        note: Some(note.clone()),
                    },
                );
                state.notify(&app, format!("{symbol}: {note}"), mint.clone());
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(period) => {}
        }
    }
}

/// Consumes the launch stream: keeps the last N, seeds token metadata, emits.
pub async fn run_launch_sink(app: AppHandle, state: Arc<AppState>, mut rx: mpsc::Receiver<LaunchEvent>) {
    let keep = state.launches_cfg.keep.max(10) as usize;
    let min_dev = (state.launches_cfg.min_dev_buy_sol.max(0.0) * 1e9).round() as u64;
    while let Some(ev) = rx.recv().await {
        match ev {
            LaunchEvent::State(s) => {
                *state.launch_state.lock().unwrap_or_else(|e| e.into_inner()) = s.clone();
                let _ = app.emit("launch_state", &s);
            }
            LaunchEvent::Launch(l) => {
                if l.dev_buy_lamports < min_dev {
                    continue;
                }
                // we know the name/symbol from the CreateEvent: cache it
                let meta = TokenMeta {
                    mint: l.mint.clone(),
                    symbol: l.symbol.clone(),
                    name: l.name.clone(),
                    uri: l.uri.clone(),
                    image: String::new(),
                };
                {
                    let store = state.store.clone();
                    let m = meta.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || store.lock().unwrap_or_else(|e| e.into_inner()).put_token_meta(&m)).await;
                }
                let _ = app.emit("meta", state.meta_row(&meta));
                let _ = state.meta_tx.send(l.mint.clone()); // resolver fetches + caches the image
                let tracked = state.wallets.get().await;
                let row = LaunchRow::new(&l, tracked.contains(&l.creator), state.is_watched(&l.mint));
                {
                    let mut q = state.launches.lock().unwrap_or_else(|e| e.into_inner());
                    q.push_back(l);
                    while q.len() > keep {
                        q.pop_front();
                    }
                }
                let _ = app.emit("launch", &row);
            }
        }
    }
}
