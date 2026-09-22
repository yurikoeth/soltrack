//! `soltrack.toml` — wallets, endpoints, limits, alerts, sim.
//!
//! Lookup order: `$SOLTRACK_CONFIG`, `./soltrack.toml`, `../soltrack.toml`
//! (so `cargo tauri dev` from `src-tauri/` finds the repo-root file), then
//! the per-user config dir. If none exists, a commented template is written
//! to the config dir and the app starts with no wallets.
//!
//! The app also *writes* the wallet list (track / untrack from the UI) using
//! `toml_edit`, which preserves comments and formatting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ingest::RateLimitConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub rpc_url: String,
    pub ws_url: String,
    /// Either `"address"` or `{ address = "...", label = "whale" }`.
    pub wallets: Vec<WalletEntry>,
    pub initial_backfill: u32,
    pub max_backfill: u32,
    pub commitment: String,
    pub rate_limit: RateLimit,
    /// How many rows the live feed keeps.
    pub feed_limit: u32,
    /// `error`, `warn`, `info`, `debug`, `trace` or a full `RUST_LOG` filter.
    pub log: String,
    pub alerts: Alerts,
    pub sim: Sim,
    pub discover: Discover,
    pub tokens: Tokens,
    pub watch: Watch,
    pub launches: Launches,
    pub ui: Ui,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    /// Chart link per token; `{mint}` is substituted.
    pub chart_url: String,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            chart_url: "https://dexscreener.com/solana/{mint}".into(),
        }
    }
}

/// Token-centric view and the confluence signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tokens {
    /// activity window for the tokens tab
    pub window_hours: u32,
    /// alert when this many distinct tracked wallets buy the same mint...
    pub confluence_wallets: u32,
    /// ...within this many minutes
    pub confluence_minutes: u32,
    /// don't refetch a token's on-chain state more often than this
    pub refresh_secs: u32,
}

impl Default for Tokens {
    fn default() -> Self {
        Self {
            window_hours: 24,
            confluence_wallets: 3,
            confluence_minutes: 30,
            refresh_secs: 60,
        }
    }
}

/// Token watchlist: polled on-chain state with a short history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Watch {
    pub mints: Vec<String>,
    pub poll_secs: u32,
    /// alert once when bonding-curve progress crosses this (percent; 0 = off)
    pub alert_progress_pct: f64,
    /// alert when market cap crosses this many SOL upward (0 = off)
    pub alert_mcap_sol: f64,
    pub history_points: u32,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            mints: Vec::new(),
            poll_secs: 15,
            alert_progress_pct: 90.0,
            alert_mcap_sol: 0.0,
            history_points: 240,
        }
    }
}

/// Live feed of new Pump.fun tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Launches {
    pub enabled: bool,
    /// rows kept in memory
    pub keep: u32,
    /// hide launches whose dev buy is below this (SOL; 0 = show all)
    pub min_dev_buy_sol: f64,
    /// separate, best-effort fetch budget so launches never starve wallet
    /// ingest; launches over budget are dropped
    pub requests_per_second: u32,
}

impl Default for Launches {
    fn default() -> Self {
        Self {
            enabled: true,
            keep: 200,
            min_dev_buy_sol: 0.0,
            requests_per_second: 1,
        }
    }
}

/// When to interrupt you. Only *live* swaps alert — never backfilled history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Alerts {
    pub enabled: bool,
    /// Ignore buys smaller than this (SOL). Display-unit config value; it is
    /// converted to lamports once at load and never used in arithmetic.
    pub min_buy_sol: f64,
    /// Flag a buy as "big" when it is at least this many times the wallet's
    /// median buy (needs a few buys of history first). 0 disables.
    pub big_buy_multiple: f64,
    pub on_sell: bool,
    /// OS notification (toast) in addition to the in-app alert.
    pub notify: bool,
    pub sound: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            enabled: true,
            min_buy_sol: 0.25,
            big_buy_multiple: 3.0,
            on_sell: false,
            notify: true,
            sound: true,
        }
    }
}

impl Alerts {
    pub fn min_buy_lamports(&self) -> u64 {
        sol_to_lamports(self.min_buy_sol)
    }

    /// `big_buy_multiple` in tenths, for integer comparison.
    pub fn big_buy_multiple_tenths(&self) -> u32 {
        (self.big_buy_multiple.max(0.0) * 10.0).round() as u32
    }
}

/// Paper-trading: what copying each wallet would have returned. Display-unit
/// values here; converted to the integer `sim::SimConfig` once at load.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sim {
    pub enabled: bool,
    /// seconds between the target's trade and our fill
    pub delay_secs: u32,
    /// price worsening on our buys and sells, percent
    pub slippage_pct: f64,
    /// `fixed` (size_sol per copied buy) or `fraction` (fraction of the
    /// target's buy, capped at cap_sol)
    pub mode: String,
    pub size_sol: f64,
    pub fraction: f64,
    pub cap_sol: f64,
    pub mirror_sells: bool,
    /// exit fully at this loss / gain, percent (0 = off)
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    /// ignore target buys smaller than this
    pub min_target_buy_sol: f64,
}

impl Default for Sim {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_secs: 15,
            slippage_pct: 2.0,
            mode: "fixed".into(),
            size_sol: 0.1,
            fraction: 0.5,
            cap_sol: 0.5,
            mirror_sells: true,
            stop_loss_pct: 0.0,
            take_profit_pct: 0.0,
            min_target_buy_sol: 0.05,
        }
    }
}

impl Sim {
    pub fn to_sim_config(&self) -> sim::SimConfig {
        let pct_bp = |p: f64| (p.max(0.0) * 100.0).round() as u32;
        sim::SimConfig {
            delay_secs: self.delay_secs,
            slippage_bp: pct_bp(self.slippage_pct),
            sizing: if self.mode.eq_ignore_ascii_case("fraction") {
                sim::Sizing::Fraction {
                    bp: pct_bp(self.fraction * 100.0),
                    cap_lamports: sol_to_lamports(self.cap_sol),
                }
            } else {
                sim::Sizing::Fixed {
                    lamports: sol_to_lamports(self.size_sol),
                }
            },
            mirror_sells: self.mirror_sells,
            stop_loss_bp: (self.stop_loss_pct > 0.0).then(|| pct_bp(self.stop_loss_pct)),
            take_profit_bp: (self.take_profit_pct > 0.0).then(|| pct_bp(self.take_profit_pct)),
            min_target_buy_lamports: sol_to_lamports(self.min_target_buy_sol),
        }
    }

    /// Short human description for the UI, e.g. "15s · 2% slip · 0.10◎/buy".
    pub fn describe(&self) -> String {
        let size = if self.mode.eq_ignore_ascii_case("fraction") {
            format!("{:.0}% of target ≤{:.2}◎", self.fraction * 100.0, self.cap_sol)
        } else {
            format!("{:.2}◎/buy", self.size_sol)
        };
        format!("{}s · {}% slip · {}", self.delay_secs, self.slippage_pct, size)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Discover {
    /// pages of 1000 signatures to walk back on a mint (caps busy tokens)
    pub max_pages: u32,
    /// oldest transactions to decode
    pub window: u32,
}

impl Default for Discover {
    fn default() -> Self {
        Self {
            max_pages: 100,
            window: 60,
        }
    }
}

fn sol_to_lamports(sol: f64) -> u64 {
    (sol.max(0.0) * 1e9).round() as u64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WalletEntry {
    Address(String),
    Labeled {
        address: String,
        #[serde(default)]
        label: Option<String>,
    },
}

impl WalletEntry {
    pub fn address(&self) -> &str {
        match self {
            WalletEntry::Address(a) => a,
            WalletEntry::Labeled { address, .. } => address,
        }
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            WalletEntry::Address(_) => None,
            WalletEntry::Labeled { label, .. } => label.as_deref().filter(|l| !l.trim().is_empty()),
        }
    }
}

impl AppConfig {
    pub fn addresses(&self) -> Vec<String> {
        self.wallets.iter().map(|w| w.address().to_string()).collect()
    }

    pub fn labels(&self) -> HashMap<String, String> {
        self.wallets
            .iter()
            .filter_map(|w| w.label().map(|l| (w.address().to_string(), l.to_string())))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimit {
    pub requests_per_second: u32,
    pub burst: u32,
}

impl Default for RateLimit {
    fn default() -> Self {
        let d = RateLimitConfig::default();
        Self {
            requests_per_second: d.requests_per_second,
            burst: d.burst,
        }
    }
}

impl From<RateLimit> for RateLimitConfig {
    fn from(r: RateLimit) -> Self {
        RateLimitConfig {
            requests_per_second: r.requests_per_second,
            burst: r.burst,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            rpc_url: "https://api.mainnet-beta.solana.com".into(),
            ws_url: "wss://api.mainnet-beta.solana.com".into(),
            wallets: Vec::new(),
            initial_backfill: 25,
            max_backfill: 500,
            commitment: "confirmed".into(),
            rate_limit: RateLimit::default(),
            feed_limit: 200,
            log: "info,soltrack=debug,ingest=debug,decode=debug".into(),
            alerts: Alerts::default(),
            sim: Sim::default(),
            discover: Discover::default(),
            tokens: Tokens::default(),
            watch: Watch::default(),
            launches: Launches::default(),
            ui: Ui::default(),
        }
    }
}

pub const TEMPLATE: &str = r#"# soltrack configuration
# Wallets to follow (base58). Plain strings or tables with a display label.
# The app appends here when you press "track" in the discover tab.
wallets = [
  # "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
  # { address = "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU", label = "whale" },
]

# Any standard Solana JSON-RPC provider. The public endpoint works but is
# rate-limited hard; a free Helius/QuickNode/Triton key is a big upgrade.
rpc_url = "https://api.mainnet-beta.solana.com"
ws_url  = "wss://api.mainnet-beta.solana.com"

# How many past signatures to pull for a wallet we've never seen, and the
# hard cap per wallet per reconnect.
initial_backfill = 25
max_backfill = 500

commitment = "confirmed"
feed_limit = 200
log = "info,soltrack=debug,ingest=debug,decode=debug"

[rate_limit]
requests_per_second = 4
burst = 8

# Live-swap alerts (backfilled history never alerts).
[alerts]
enabled = true
min_buy_sol = 0.25        # ignore buys smaller than this
big_buy_multiple = 3.0    # "big buy" = at least N x the wallet's median buy (0 = off)
on_sell = false
notify = true             # OS toast in addition to the in-app alert
sound = true

# Paper trading: what copying each wallet would have returned.
[sim]
enabled = true
delay_secs = 15           # our fill lands this long after the target's trade
slippage_pct = 2.0        # worse price on our buys and sells
mode = "fixed"            # "fixed" = size_sol per buy; "fraction" = fraction of target's buy, capped
size_sol = 0.1
fraction = 0.5
cap_sol = 0.5
mirror_sells = true
stop_loss_pct = 0         # 0 = off
take_profit_pct = 0       # 0 = off
min_target_buy_sol = 0.05

# Wallet discovery ("who bought this token first").
[discover]
max_pages = 100           # pages of 1000 signatures to walk back on the mint (1 RPC call each)
window = 60               # oldest transactions to decode

# Tokens tab + confluence ("N of my wallets bought the same coin within M minutes").
[tokens]
window_hours = 24
confluence_wallets = 3
confluence_minutes = 30
refresh_secs = 60         # min interval between on-chain state refreshes per token

# Token watchlist: polled bonding-curve / pool state. The app edits `mints`
# when you press watch / unwatch.
[watch]
mints = []
poll_secs = 15
alert_progress_pct = 90   # alert once when a curve passes this (0 = off)
alert_mcap_sol = 0        # alert when mcap crosses this many SOL upward (0 = off)
history_points = 240

# Live feed of new Pump.fun launches (one extra WebSocket subscription).
[launches]
enabled = true
keep = 200
min_dev_buy_sol = 0       # hide launches with a smaller dev buy (0 = show all)
requests_per_second = 1   # separate best-effort budget; launches over it are dropped

[ui]
chart_url = "https://dexscreener.com/solana/{mint}"   # or https://gmgn.ai/sol/token/{mint}
"#;

pub struct Loaded {
    pub config: AppConfig,
    pub path: PathBuf,
    pub created: bool,
}

/// `canonicalize` on Windows returns a verbatim-prefixed path; use the plain form for display.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str() {
        Some(s) if s.starts_with(r"\\?\") && !s.starts_with(r"\\?\UNC\") => PathBuf::from(&s[4..]),
        _ => path,
    }
}

pub fn load(config_dir: &Path) -> anyhow::Result<Loaded> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("SOLTRACK_CONFIG") {
        candidates.push(PathBuf::from(p));
    }
    candidates.push(PathBuf::from("soltrack.toml"));
    candidates.push(PathBuf::from("../soltrack.toml"));
    candidates.push(config_dir.join("soltrack.toml"));

    for path in &candidates {
        if path.is_file() {
            let text = std::fs::read_to_string(path)?;
            let mut config: AppConfig = toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            config.wallets.retain(|w| !w.address().trim().is_empty());
            for w in &config.wallets {
                validate_pubkey(w.address())
                    .map_err(|e| anyhow::anyhow!("{}: wallet {:?}: {e}", path.display(), w.address()))?;
            }
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            let path = strip_verbatim(path);
            return Ok(Loaded {
                config,
                path,
                created: false,
            });
        }
    }

    let path = config_dir.join("soltrack.toml");
    std::fs::create_dir_all(config_dir)?;
    std::fs::write(&path, TEMPLATE)?;
    Ok(Loaded {
        config: AppConfig::default(),
        path,
        created: true,
    })
}

/// Append a wallet to the `wallets` array in the config file, preserving
/// everything else. No-op if already present.
pub fn add_wallet_to_file(path: &Path, address: &str, label: Option<&str>) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let arr = doc
        .entry("wallets")
        .or_insert(toml_edit::value(toml_edit::Array::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("`wallets` is not an array"))?;
    if arr.iter().any(|v| entry_address(v) == Some(address)) {
        return Ok(false);
    }
    let item: toml_edit::Value = match label.filter(|l| !l.trim().is_empty()) {
        Some(l) => {
            let mut t = toml_edit::InlineTable::new();
            t.insert("address", address.into());
            t.insert("label", l.trim().into());
            t.into()
        }
        None => address.into(),
    };
    arr.push_formatted(item);
    // one entry per line, like the template
    for v in arr.iter_mut() {
        v.decor_mut().set_prefix("\n  ");
    }
    arr.set_trailing("\n");
    arr.set_trailing_comma(true);
    std::fs::write(path, doc.to_string())?;
    Ok(true)
}

pub fn remove_wallet_from_file(path: &Path, address: &str) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let Some(arr) = doc.get_mut("wallets").and_then(|w| w.as_array_mut()) else {
        return Ok(false);
    };
    let before = arr.len();
    arr.retain(|v| entry_address(v) != Some(address));
    if arr.len() == before {
        return Ok(false);
    }
    std::fs::write(path, doc.to_string())?;
    Ok(true)
}

/// Append a mint to `[watch] mints`, creating the table if needed.
pub fn add_watch_to_file(path: &Path, mint: &str) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let watch = doc
        .entry("watch")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let table = watch
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`watch` is not a table"))?;
    let arr = table
        .entry("mints")
        .or_insert(toml_edit::value(toml_edit::Array::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("`watch.mints` is not an array"))?;
    if arr.iter().any(|v| v.as_str() == Some(mint)) {
        return Ok(false);
    }
    arr.push(mint);
    std::fs::write(path, doc.to_string())?;
    Ok(true)
}

pub fn remove_watch_from_file(path: &Path, mint: &str) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let Some(arr) = doc
        .get_mut("watch")
        .and_then(|w| w.get_mut("mints"))
        .and_then(|m| m.as_array_mut())
    else {
        return Ok(false);
    };
    let before = arr.len();
    arr.retain(|v| v.as_str() != Some(mint));
    if arr.len() == before {
        return Ok(false);
    }
    std::fs::write(path, doc.to_string())?;
    Ok(true)
}

fn entry_address(v: &toml_edit::Value) -> Option<&str> {
    match v {
        toml_edit::Value::String(s) => Some(s.value().as_str()),
        toml_edit::Value::InlineTable(t) => t.get("address").and_then(|a| a.as_str()),
        _ => None,
    }
}

pub fn validate_pubkey(s: &str) -> Result<(), String> {
    let bytes = bs58_decode(s).ok_or("not base58")?;
    if bytes.len() != 32 {
        return Err(format!("decodes to {} bytes, expected 32", bytes.len()));
    }
    Ok(())
}

// Tiny base58 decoder so this crate doesn't need another dependency just
// for validation.
fn bs58_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut out: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = ALPHABET.iter().position(|&a| a == c)? as u32;
        for b in out.iter_mut() {
            carry += *b as u32 * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let leading = s.bytes().take_while(|&c| c == b'1').count();
    out.extend(std::iter::repeat(0).take(leading));
    out.reverse();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    const B: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

    #[test]
    fn template_parses_to_defaults() {
        let c: AppConfig = toml::from_str(TEMPLATE).unwrap();
        assert!(c.wallets.is_empty());
        assert_eq!(c.rate_limit.requests_per_second, 4);
        assert_eq!(c.initial_backfill, 25);
        assert!(c.alerts.enabled);
        assert_eq!(c.alerts.min_buy_lamports(), 250_000_000);
        assert_eq!(c.alerts.big_buy_multiple_tenths(), 30);
        let s = c.sim.to_sim_config();
        assert_eq!(s.delay_secs, 15);
        assert_eq!(s.slippage_bp, 200);
        assert_eq!(s.sizing, sim::Sizing::Fixed { lamports: 100_000_000 });
        assert_eq!(s.stop_loss_bp, None);
        assert_eq!(s.min_target_buy_lamports, 50_000_000);
        assert_eq!(c.discover.window, 60);
        assert_eq!(c.tokens.confluence_wallets, 3);
        assert_eq!(c.watch.poll_secs, 15);
        assert!(c.launches.enabled);
    }

    #[test]
    fn watch_list_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("soltrack-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("soltrack.toml");
        // a file WITHOUT a [watch] table: it must be created
        std::fs::write(&path, "wallets = []\nrpc_url = \"x\"\n").unwrap();
        assert!(add_watch_to_file(&path, A).unwrap());
        assert!(!add_watch_to_file(&path, A).unwrap());
        assert!(add_watch_to_file(&path, B).unwrap());
        let c: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.watch.mints, vec![A.to_string(), B.to_string()]);
        assert_eq!(c.rpc_url, "x");
        assert!(remove_watch_from_file(&path, A).unwrap());
        assert!(!remove_watch_from_file(&path, A).unwrap());
        let c: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.watch.mints, vec![B.to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wallets_accept_strings_and_labeled_tables() {
        let c: AppConfig = toml::from_str(
            r#"
            wallets = [
              "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
              { address = "11111111111111111111111111111111", label = "sys" },
              { address = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA" },
            ]
            "#,
        )
        .unwrap();
        assert_eq!(c.addresses().len(), 3);
        let labels = c.labels();
        assert_eq!(labels.get("11111111111111111111111111111111").map(String::as_str), Some("sys"));
        assert_eq!(labels.len(), 1);
    }

    #[test]
    fn pubkey_validation() {
        assert!(validate_pubkey(A).is_ok());
        assert!(validate_pubkey("11111111111111111111111111111111").is_ok());
        assert!(validate_pubkey("not-a-key").is_err());
        assert!(validate_pubkey("abc").is_err());
    }

    #[test]
    fn add_and_remove_wallets_preserve_the_rest_of_the_file() {
        let dir = std::env::temp_dir().join(format!("soltrack-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("soltrack.toml");
        std::fs::write(&path, TEMPLATE).unwrap();

        assert!(add_wallet_to_file(&path, A, Some("whale")).unwrap());
        assert!(add_wallet_to_file(&path, B, None).unwrap());
        assert!(!add_wallet_to_file(&path, B, Some("dup")).unwrap(), "duplicate must be a no-op");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# soltrack configuration"), "comments preserved");
        assert!(text.contains("[alerts]"));
        let c: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(c.addresses(), vec![A.to_string(), B.to_string()]);
        assert_eq!(c.labels().get(A).map(String::as_str), Some("whale"));

        assert!(remove_wallet_from_file(&path, A).unwrap());
        assert!(!remove_wallet_from_file(&path, A).unwrap());
        let c: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.addresses(), vec![B.to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
