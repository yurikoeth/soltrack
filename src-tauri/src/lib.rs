mod config;
mod dto;
mod pipeline;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use ingest::{IngestConfig, RateLimiter, RpcClient, WalletSet, WsLogsTransport};
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use dto::{CandidateRow, DiscoverResult, Snapshot, SwapRow, TokenRow, UnknownRow, WalletRow, WatchRow};
use pipeline::{AppState, StoreCursor};
use store::Storage;

type State<'a> = tauri::State<'a, Arc<AppState>>;

#[tauri::command]
async fn snapshot(state: State<'_>) -> Result<Snapshot, String> {
    let wallets = state.wallets.get().await;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (feed, unknown, meta) = {
            let store = st.store.lock().unwrap_or_else(|e| e.into_inner());
            let feed = store
                .recent_swaps(st.feed_limit)
                .map_err(|e| e.to_string())?
                .iter()
                .map(SwapRow::from)
                .collect();
            let unknown = store
                .recent_unknown(50)
                .map_err(|e| e.to_string())?
                .iter()
                .map(UnknownRow::from)
                .collect();
            let meta = store
                .all_token_meta()
                .map_err(|e| e.to_string())?
                .iter()
                .map(|m| st.meta_row(m))
                .collect();
            (feed, unknown, meta)
        };
        Ok(Snapshot {
            wallets: st.wallet_rows(&wallets),
            feed,
            unknown,
            alerts: st.recent_alerts(),
            meta,
            status: st.status.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            config_path: st.config_path.display().to_string(),
            config_created: st.config_created,
            config_error: st.config_error.clone(),
            rpc_url: st.rpc_url.clone(),
            fetch_failures: st.fetch_failures.load(Ordering::Relaxed),
            alerts_enabled: st.alerts_cfg.enabled,
            sound: st.alerts_cfg.sound,
            muted: st.muted.load(Ordering::Relaxed),
            sim_enabled: st.sim_cfg.is_some(),
            sim_describe: st.sim_describe.clone(),
            tokens_window_hours: st.tokens_cfg.window_hours,
            confluence_wallets: st.tokens_cfg.confluence_wallets,
            confluence_minutes: st.tokens_cfg.confluence_minutes,
            watch: st.watch_rows(),
            launches: st.launch_rows(&wallets),
            launches_enabled: st.launches_cfg.enabled,
            launch_state: st.launch_state.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            chart_url: st.chart_url.clone(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Webview-side diagnostics land in the app log.
#[tauri::command]
fn ui_log(level: String, msg: String) {
    match level.as_str() {
        "warn" => tracing::warn!(target: "ui", "{msg}"),
        "error" => tracing::error!(target: "ui", "{msg}"),
        _ => tracing::info!(target: "ui", "{msg}"),
    }
}

/// Mute silences OS notifications (Rust side) and sound (webview side).
#[tauri::command]
fn set_muted(state: State<'_>, muted: bool) -> bool {
    state.muted.store(muted, Ordering::Relaxed);
    muted
}

/// Tokens tab: activity of tracked wallets per mint over the configured window.
#[tauri::command]
async fn tokens(state: State<'_>) -> Result<Vec<TokenRow>, String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || st.token_rows().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

/// Ask for a token's on-chain state now (tokens tab row expand, etc.).
#[tauri::command]
fn refresh_token(state: State<'_>, mint: String) {
    let _ = state.token_tx.send(mint);
}

/// Earliest buyers of a mint. Progress goes out as `discover_progress` events.
#[tauri::command]
async fn discover(app: tauri::AppHandle, state: State<'_>, mint: String) -> Result<DiscoverResult, String> {
    let mint = mint.trim().to_string();
    config::validate_pubkey(&mint).map_err(|e| format!("not a mint address: {e}"))?;
    let tracked = state.wallets.get().await;
    let app2 = app.clone();
    let found = ingest::discover::early_buyers(&state.rpc, &mint, &state.discover_cfg, move |p| {
        let _ = app2.emit("discover_progress", p);
    })
    .await
    .map_err(|e| e.to_string())?;
    tracing::info!(%mint, candidates = found.candidates.len(), scanned = found.scanned, reached_launch = found.reached_launch, "discovery finished");
    Ok(DiscoverResult {
        mint: mint.clone(),
        candidates: found
            .candidates
            .iter()
            .map(|c| CandidateRow::new(c, tracked.contains(&c.wallet)))
            .collect(),
        reached_launch: found.reached_launch,
        scanned: found.scanned,
        launch_slot: found.launch_slot,
        launch_time: found.launch_time,
        max_pages: state.discover_cfg.max_pages,
    })
}

/// Add a wallet: config file, live set (ingest resubscribes + backfills), UI.
#[tauri::command]
async fn track_wallet(app: tauri::AppHandle, state: State<'_>, address: String, label: Option<String>) -> Result<Vec<WalletRow>, String> {
    let address = address.trim().to_string();
    config::validate_pubkey(&address).map_err(|e| format!("not a wallet address: {e}"))?;
    let label = label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    config::add_wallet_to_file(&state.config_path, &address, label.as_deref()).map_err(|e| e.to_string())?;
    if let Some(l) = &label {
        state.labels.write().unwrap_or_else(|e| e.into_inner()).insert(address.clone(), l.clone());
    }
    let added = state.wallets.add(&address).await;
    tracing::info!(%address, ?label, added, "track wallet");
    let st = state.inner().clone();
    let a = address.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || st.rebuild_wallet(&a)).await;
    emit_wallets(&app, &state).await
}

#[tauri::command]
async fn untrack_wallet(app: tauri::AppHandle, state: State<'_>, address: String) -> Result<Vec<WalletRow>, String> {
    config::remove_wallet_from_file(&state.config_path, &address).map_err(|e| e.to_string())?;
    let removed = state.wallets.remove(&address).await;
    state.forget_wallet(&address);
    tracing::info!(%address, removed, "untrack wallet");
    emit_wallets(&app, &state).await
}

/// Add a mint to the watchlist (config + poller) and fetch it once right away.
#[tauri::command]
async fn watch_mint(app: tauri::AppHandle, state: State<'_>, mint: String) -> Result<Vec<WatchRow>, String> {
    let mint = mint.trim().to_string();
    config::validate_pubkey(&mint).map_err(|e| format!("not a mint address: {e}"))?;
    config::add_watch_to_file(&state.config_path, &mint).map_err(|e| e.to_string())?;
    {
        let mut w = state.watch_mints.write().unwrap_or_else(|e| e.into_inner());
        if !w.contains(&mint) {
            w.push(mint.clone());
        }
    }
    let _ = state.meta_tx.send(mint.clone());
    tracing::info!(%mint, "watch mint");
    // seed the first point immediately so the row isn't empty
    if let Ok(ts) = ingest::tokens::fetch_token_state(&state.rpc, &mint).await {
        let point = dto::WatchPoint::from(&ts);
        let mut w = state.watch.lock().unwrap_or_else(|e| e.into_inner());
        let e = w.entry(mint.clone()).or_insert_with(|| pipeline::WatchEntry {
            points: Default::default(),
            creator: ts.curve.as_ref().and_then(|c| c.creator.clone()),
            alerted_progress: false,
            alerted_migrated: ts.curve.as_ref().map_or(false, |c| c.complete),
            above_mcap: false,
        });
        e.points.push_back(point);
        drop(w);
        state.token_states.lock().unwrap_or_else(|e| e.into_inner()).insert(mint.clone(), ts);
    }
    let rows = state.watch_rows();
    let _ = app.emit("watch", &rows);
    Ok(rows)
}

#[tauri::command]
async fn unwatch_mint(app: tauri::AppHandle, state: State<'_>, mint: String) -> Result<Vec<WatchRow>, String> {
    config::remove_watch_from_file(&state.config_path, &mint).map_err(|e| e.to_string())?;
    state.watch_mints.write().unwrap_or_else(|e| e.into_inner()).retain(|m| m != &mint);
    state.watch.lock().unwrap_or_else(|e| e.into_inner()).remove(&mint);
    tracing::info!(%mint, "unwatch mint");
    let rows = state.watch_rows();
    let _ = app.emit("watch", &rows);
    Ok(rows)
}

async fn emit_wallets(app: &tauri::AppHandle, state: &State<'_>) -> Result<Vec<WalletRow>, String> {
    let wallets = state.wallets.get().await;
    let st = state.inner().clone();
    let rows = tauri::async_runtime::spawn_blocking(move || st.wallet_rows(&wallets))
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("wallets", &rows);
    Ok(rows)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            set_muted,
            ui_log,
            tokens,
            refresh_token,
            discover,
            track_wallet,
            untrack_wallet,
            watch_mint,
            unwatch_mint
        ])
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let (loaded, config_error) = match config::load(&config_dir) {
                Ok(l) => (l, None),
                Err(e) => (
                    config::Loaded {
                        config: Default::default(),
                        path: config_dir.join("soltrack.toml"),
                        created: false,
                    },
                    Some(e.to_string()),
                ),
            };
            init_logging(&loaded.config.log);
            if let Some(e) = &config_error {
                tracing::error!(error = %e, "config failed to load; running with defaults");
            } else if loaded.created {
                tracing::warn!(path = %loaded.path.display(), "no config found; wrote a template — add wallets (or use the discover tab)");
            } else {
                tracing::info!(path = %loaded.path.display(), wallets = loaded.config.wallets.len(), "config loaded");
            }
            let cfg = loaded.config;
            let addresses = cfg.addresses();

            let db_path = data_dir.join("soltrack.db");
            tracing::info!(path = %db_path.display(), "opening store");
            let store = Arc::new(Mutex::new(store::SqliteStorage::open(&db_path)?));

            let ingest_cfg = IngestConfig {
                rpc_url: cfg.rpc_url.clone(),
                ws_url: cfg.ws_url.clone(),
                initial_backfill: cfg.initial_backfill,
                max_backfill: cfg.max_backfill,
                rate: cfg.rate_limit.into(),
                commitment: cfg.commitment.clone(),
            };
            let limiter = Arc::new(RateLimiter::new(ingest_cfg.rate));
            let rpc = Arc::new(RpcClient::new(&ingest_cfg.rpc_url, limiter));
            let wallet_set = Arc::new(WalletSet::new(addresses.clone()));

            let (meta_tx, meta_rx) = mpsc::unbounded_channel::<String>();
            let (token_tx, token_rx) = mpsc::unbounded_channel::<String>();
            let mut watch_mints = cfg.watch.mints.clone();
            watch_mints.retain(|m| config::validate_pubkey(m).is_ok());
            let state = Arc::new(AppState {
                store: store.clone(),
                wallets: wallet_set.clone(),
                labels: std::sync::RwLock::new(cfg.labels()),
                pnl: Mutex::new(Default::default()),
                sims: Mutex::new(Default::default()),
                sim_cfg: cfg.sim.enabled.then(|| cfg.sim.to_sim_config()),
                sim_describe: cfg.sim.describe(),
                status: Mutex::new(ingest::ConnState::Connecting),
                fetch_failures: Default::default(),
                config_path: loaded.path.clone(),
                config_created: loaded.created,
                config_error,
                rpc_url: cfg.rpc_url.clone(),
                feed_limit: cfg.feed_limit,
                meta_tx: meta_tx.clone(),
                token_tx: token_tx.clone(),
                alerts_cfg: cfg.alerts.clone(),
                muted: Default::default(),
                alerts: Default::default(),
                alert_seq: Default::default(),
                rpc: rpc.clone(),
                discover_cfg: ingest::DiscoverConfig {
                    max_pages: cfg.discover.max_pages,
                    window: cfg.discover.window,
                    commitment: cfg.commitment.clone(),
                },
                tokens_cfg: cfg.tokens.clone(),
                token_states: Default::default(),
                confluence_last: Default::default(),
                watch_cfg: cfg.watch.clone(),
                watch_mints: std::sync::RwLock::new(watch_mints.clone()),
                watch: Default::default(),
                launches_cfg: cfg.launches.clone(),
                launches: Default::default(),
                launch_state: Mutex::new(ingest::LaunchState::Connecting),
                chart_url: cfg.ui.chart_url.clone(),
                image_dir: data_dir.join("images"),
            });
            state.rebuild_all(&addresses)?;
            app.manage(state.clone());

            let cancel = CancellationToken::new();

            // Symbols for anything already in the DB that lacks them.
            tauri::async_runtime::spawn(pipeline::run_meta_resolver(
                app.handle().clone(),
                store.clone(),
                rpc.clone(),
                state.image_dir.clone(),
                meta_rx,
            ));
            for mint in store.lock().unwrap_or_else(|e| e.into_inner()).mints_missing_meta()? {
                let _ = meta_tx.send(mint);
            }
            for mint in store.lock().unwrap_or_else(|e| e.into_inner()).mints_missing_image()? {
                let _ = meta_tx.send(mint);
            }
            // images known but not yet cached on disk
            for m in store.lock().unwrap_or_else(|e| e.into_inner()).all_token_meta()? {
                if !m.image.is_empty() && state.local_image(&m.mint).is_none() {
                    let _ = meta_tx.send(m.mint);
                }
            }
            for mint in &watch_mints {
                let _ = meta_tx.send(mint.clone());
            }

            // On-chain token state, refreshed as tokens trade.
            tauri::async_runtime::spawn(pipeline::run_token_refresher(app.handle().clone(), state.clone(), token_rx));
            {
                // seed state for tokens with open positions
                let map = state.pnl.lock().unwrap_or_else(|e| e.into_inner());
                for w in map.values() {
                    for p in w.open_positions() {
                        let _ = token_tx.send(p.mint.clone());
                    }
                }
            }

            // Watchlist poller.
            tauri::async_runtime::spawn(pipeline::run_watch_poller(app.handle().clone(), state.clone(), cancel.clone()));

            // Launch feed.
            if cfg.launches.enabled {
                let (ltx, lrx) = mpsc::channel(256);
                tauri::async_runtime::spawn(pipeline::run_launch_sink(app.handle().clone(), state.clone(), lrx));
                let budget = Arc::new(RateLimiter::new(ingest::RateLimitConfig {
                    requests_per_second: cfg.launches.requests_per_second.max(1),
                    burst: 3,
                }));
                tauri::async_runtime::spawn(ingest::launches::run(
                    cfg.ws_url.clone(),
                    cfg.commitment.clone(),
                    rpc.clone(),
                    budget,
                    ltx,
                    cancel.clone(),
                ));
            } else {
                *state.launch_state.lock().unwrap_or_else(|e| e.into_inner()) = ingest::LaunchState::Disconnected {
                    reason: "disabled in config".into(),
                    retry_in_ms: 0,
                };
            }

            // Wallet ingest.
            let transport = Box::new(WsLogsTransport::new(&ingest_cfg.ws_url, &ingest_cfg.commitment));
            let cursor = Arc::new(StoreCursor(store.clone()));
            let (tx, rx) = mpsc::channel(1024);
            tauri::async_runtime::spawn(pipeline::run(app.handle().clone(), state.clone(), rx));
            tauri::async_runtime::spawn(ingest::run(
                ingest_cfg,
                transport,
                rpc,
                cursor,
                wallet_set,
                tx,
                cancel.clone(),
            ));
            app.manage(cancel);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(cancel) = window.try_state::<CancellationToken>() {
                    cancel.cancel();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn init_logging(filter: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| filter.to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&filter).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(true)
        .compact()
        .try_init();
}
