//! SQLite persistence for decoded swaps, behind the [`Storage`] trait.
//!
//! Single-writer design: one `SqliteStorage` owns one connection in WAL mode.
//! The API is synchronous; async callers wrap it in `spawn_blocking`.
//! Every write is idempotent on `(signature, ix_index)` so backfill can
//! replay ranges blindly without double-counting.

mod schema;

use decode::{Side, SwapEvent, TokenMeta, UnknownSwap, Venue};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration: {0}")]
    Migration(String),
    #[error("amount {0} does not fit in SQLite INTEGER (i64)")]
    AmountOverflow(u64),
    #[error("corrupt row: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Result of an insert attempt — lets callers know whether to emit a UI event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    New,
    Duplicate,
}

/// Persistence boundary.
pub trait Storage: Send + 'static {
    /// Idempotent on `(signature, ix_index)`. Also records the signature as
    /// processed. Returns `Duplicate` without error on replay.
    fn insert_swap(&mut self, ev: &SwapEvent) -> Result<Inserted>;

    /// Records an `UnknownSwap` for the audit log. Idempotent on signature.
    fn insert_unknown(&mut self, u: &UnknownSwap) -> Result<Inserted>;

    /// Records a signature we fetched and classified as not a swap, so it is
    /// never fetched again.
    fn mark_not_swap(&mut self, signature: &str, wallet: &str, slot: u64) -> Result<Inserted>;

    /// All swaps for a wallet, ordered `(slot, ix_index)` ASC — what pnl replays.
    fn swaps_for_wallet(&self, wallet: &str) -> Result<Vec<SwapEvent>>;

    /// Most recent `limit` swaps across all wallets, newest first (live feed).
    fn recent_swaps(&self, limit: u32) -> Result<Vec<SwapEvent>>;

    /// All swaps (any wallet) with `block_time >= since`, oldest first.
    fn swaps_since(&self, since: i64) -> Result<Vec<SwapEvent>>;

    /// Most recent `limit` unknown swaps, newest first.
    fn recent_unknown(&self, limit: u32) -> Result<Vec<UnknownSwap>>;

    /// Highest slot fully processed for this wallet (any outcome) — the
    /// backfill cursor.
    fn last_seen_slot(&self, wallet: &str) -> Result<Option<u64>>;

    /// Has this signature been processed (any outcome)?
    fn has_signature(&self, signature: &str) -> Result<bool>;

    /// Last known decimals for a mint.
    fn mint_decimals(&self, mint: &str) -> Result<Option<u8>>;
    fn put_mint_decimals(&mut self, mint: &str, decimals: u8) -> Result<()>;

    /// Cached display metadata for a mint (a row with an empty symbol means
    /// "looked up, chain has none").
    fn token_meta(&self, mint: &str) -> Result<Option<TokenMeta>>;
    fn put_token_meta(&mut self, meta: &TokenMeta) -> Result<()>;
    fn all_token_meta(&self) -> Result<Vec<TokenMeta>>;
    /// Mints that appear in `swaps` but have no `token_meta` row yet.
    fn mints_missing_meta(&self) -> Result<Vec<String>>;
    /// Mints whose metadata has a `uri` but no image resolved yet.
    fn mints_missing_image(&self) -> Result<Vec<String>>;
}

pub struct SqliteStorage {
    conn: Connection,
}

impl SqliteStorage {
    /// Open (creating if needed) and migrate.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// In-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    fn mark_processed(
        &mut self,
        signature: &str,
        wallet: &str,
        slot: u64,
        outcome: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO processed_signatures (signature, wallet, slot, outcome, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![signature, wallet, to_i64(slot)?, outcome, now()],
        )?;
        Ok(())
    }
}

impl Storage for SqliteStorage {
    fn insert_swap(&mut self, ev: &SwapEvent) -> Result<Inserted> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO processed_signatures (signature, wallet, slot, outcome, seen_at)
             VALUES (?1, ?2, ?3, 'swap', ?4)",
            params![ev.signature, ev.wallet, to_i64(ev.slot)?, now()],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO mints (mint, decimals) VALUES (?1, ?2)",
            params![ev.mint, ev.token_decimals],
        )?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO swaps
               (signature, ix_index, slot, block_time, wallet, mint, venue, side,
                token_amount, token_decimals, sol_amount, fee_lamports)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                ev.signature,
                ev.ix_index,
                to_i64(ev.slot)?,
                ev.block_time,
                ev.wallet,
                ev.mint,
                ev.venue.as_str(),
                ev.side.as_str(),
                to_i64(ev.token_amount)?,
                ev.token_decimals,
                to_i64(ev.sol_amount)?,
                to_i64(ev.fee_lamports)?,
            ],
        )?;
        tx.commit()?;
        Ok(if changed == 1 { Inserted::New } else { Inserted::Duplicate })
    }

    fn insert_unknown(&mut self, u: &UnknownSwap) -> Result<Inserted> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO processed_signatures (signature, wallet, slot, outcome, seen_at)
             VALUES (?1, ?2, ?3, 'unknown', ?4)",
            params![u.signature, u.wallet, to_i64(u.slot)?, now()],
        )?;
        let programs = serde_json::to_string(&u.programs).expect("Vec<String> serializes");
        let changed = tx.execute(
            "INSERT OR IGNORE INTO unknown_swaps (signature, slot, block_time, wallet, programs)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![u.signature, to_i64(u.slot)?, u.block_time, u.wallet, programs],
        )?;
        tx.commit()?;
        Ok(if changed == 1 { Inserted::New } else { Inserted::Duplicate })
    }

    fn mark_not_swap(&mut self, signature: &str, wallet: &str, slot: u64) -> Result<Inserted> {
        self.mark_processed(signature, wallet, slot, "not_swap")?;
        Ok(if self.conn.changes() == 1 { Inserted::New } else { Inserted::Duplicate })
    }

    fn swaps_for_wallet(&self, wallet: &str) -> Result<Vec<SwapEvent>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {SWAP_COLS} FROM swaps WHERE wallet = ?1 ORDER BY slot ASC, ix_index ASC"
        ))?;
        let rows = stmt.query_map(params![wallet], row_to_swap)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
            .and_then(|v| v.into_iter().collect())
    }

    fn recent_swaps(&self, limit: u32) -> Result<Vec<SwapEvent>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {SWAP_COLS} FROM swaps ORDER BY slot DESC, ix_index DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit], row_to_swap)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
            .and_then(|v| v.into_iter().collect())
    }

    fn swaps_since(&self, since: i64) -> Result<Vec<SwapEvent>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {SWAP_COLS} FROM swaps WHERE block_time >= ?1 ORDER BY slot ASC, ix_index ASC"
        ))?;
        let rows = stmt.query_map(params![since], row_to_swap)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
            .and_then(|v| v.into_iter().collect())
    }

    fn recent_unknown(&self, limit: u32) -> Result<Vec<UnknownSwap>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT signature, slot, block_time, wallet, programs
             FROM unknown_swaps ORDER BY slot DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            let programs: String = r.get(4)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
                programs,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (signature, slot, block_time, wallet, programs) = r?;
            out.push(UnknownSwap {
                signature,
                slot: from_i64(slot)?,
                block_time,
                wallet,
                programs: serde_json::from_str(&programs)
                    .map_err(|e| StoreError::Corrupt(format!("programs json: {e}")))?,
            });
        }
        Ok(out)
    }

    fn last_seen_slot(&self, wallet: &str) -> Result<Option<u64>> {
        let slot: Option<i64> = self.conn.query_row(
            "SELECT MAX(slot) FROM processed_signatures WHERE wallet = ?1",
            params![wallet],
            |r| r.get(0),
        )?;
        slot.map(from_i64).transpose()
    }

    fn has_signature(&self, signature: &str) -> Result<bool> {
        let hit: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM processed_signatures WHERE signature = ?1",
                params![signature],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    fn mint_decimals(&self, mint: &str) -> Result<Option<u8>> {
        let d: Option<u8> = self
            .conn
            .query_row("SELECT decimals FROM mints WHERE mint = ?1", params![mint], |r| r.get(0))
            .optional()?;
        Ok(d)
    }

    fn put_mint_decimals(&mut self, mint: &str, decimals: u8) -> Result<()> {
        self.conn.execute(
            "INSERT INTO mints (mint, decimals) VALUES (?1, ?2)
             ON CONFLICT(mint) DO UPDATE SET decimals = excluded.decimals",
            params![mint, decimals],
        )?;
        Ok(())
    }

    fn token_meta(&self, mint: &str) -> Result<Option<TokenMeta>> {
        self.conn
            .query_row(
                "SELECT mint, symbol, name, uri, image FROM token_meta WHERE mint = ?1",
                params![mint],
                row_to_meta,
            )
            .optional()
            .map_err(Into::into)
    }

    fn put_token_meta(&mut self, meta: &TokenMeta) -> Result<()> {
        self.conn.execute(
            "INSERT INTO token_meta (mint, symbol, name, uri, image, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(mint) DO UPDATE SET symbol = excluded.symbol, name = excluded.name,
                                             uri = excluded.uri, image = excluded.image,
                                             fetched_at = excluded.fetched_at",
            params![meta.mint, meta.symbol, meta.name, meta.uri, meta.image, now()],
        )?;
        Ok(())
    }

    fn all_token_meta(&self) -> Result<Vec<TokenMeta>> {
        let mut stmt = self.conn.prepare_cached("SELECT mint, symbol, name, uri, image FROM token_meta")?;
        let rows = stmt.query_map([], row_to_meta)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn mints_missing_image(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT mint FROM token_meta WHERE uri != '' AND image = '' ORDER BY fetched_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn mints_missing_meta(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT mint FROM swaps WHERE mint NOT IN (SELECT mint FROM token_meta)
             GROUP BY mint ORDER BY MAX(slot) DESC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn row_to_meta(r: &Row<'_>) -> rusqlite::Result<TokenMeta> {
    Ok(TokenMeta {
        mint: r.get(0)?,
        symbol: r.get(1)?,
        name: r.get(2)?,
        uri: r.get(3)?,
        image: r.get(4)?,
    })
}

const SWAP_COLS: &str = "signature, ix_index, slot, block_time, wallet, mint, venue, side, \
                         token_amount, token_decimals, sol_amount, fee_lamports";

fn row_to_swap(r: &Row<'_>) -> rusqlite::Result<Result<SwapEvent>> {
    let venue: String = r.get(6)?;
    let side: String = r.get(7)?;
    let slot: i64 = r.get(2)?;
    let token_amount: i64 = r.get(8)?;
    let sol_amount: i64 = r.get(10)?;
    let fee_lamports: i64 = r.get(11)?;
    Ok((|| {
        Ok(SwapEvent {
            signature: r.get(0)?,
            ix_index: r.get(1)?,
            slot: from_i64(slot)?,
            block_time: r.get(3)?,
            wallet: r.get(4)?,
            mint: r.get(5)?,
            venue: Venue::parse(&venue)
                .ok_or_else(|| StoreError::Corrupt(format!("venue {venue:?}")))?,
            side: Side::parse(&side).ok_or_else(|| StoreError::Corrupt(format!("side {side:?}")))?,
            token_amount: from_i64(token_amount)?,
            token_decimals: r.get(9)?,
            sol_amount: from_i64(sol_amount)?,
            fee_lamports: from_i64(fee_lamports)?,
        })
    })())
}

/// SQLite INTEGER is i64. Lamports and 6-decimal token supplies fit with
/// room to spare; refuse loudly rather than wrap if something doesn't.
fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StoreError::AmountOverflow(v))
}

fn from_i64(v: i64) -> Result<u64> {
    u64::try_from(v).map_err(|_| StoreError::Corrupt(format!("negative amount {v}")))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swap(sig: &str, ix: u16, slot: u64, wallet: &str, side: Side) -> SwapEvent {
        SwapEvent {
            signature: sig.into(),
            slot,
            block_time: Some(1_788_906_323),
            wallet: wallet.into(),
            mint: "MintAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            venue: Venue::PumpFunCurve,
            side,
            token_amount: 1_000_000,
            token_decimals: 6,
            sol_amount: 50_000_000,
            fee_lamports: 500_000,
            ix_index: ix,
        }
    }

    #[test]
    fn insert_is_idempotent_on_signature_and_ix_index() {
        let mut s = SqliteStorage::open_in_memory().unwrap();
        let a = swap("sigA", 0, 100, "W1", Side::Buy);
        assert_eq!(s.insert_swap(&a).unwrap(), Inserted::New);
        assert_eq!(s.insert_swap(&a).unwrap(), Inserted::Duplicate);
        // same signature, different instruction → a distinct swap
        let b = swap("sigA", 3, 100, "W1", Side::Sell);
        assert_eq!(s.insert_swap(&b).unwrap(), Inserted::New);
        assert_eq!(s.swaps_for_wallet("W1").unwrap().len(), 2);
        assert!(s.has_signature("sigA").unwrap());
        assert!(!s.has_signature("sigB").unwrap());
    }

    #[test]
    fn swaps_round_trip_and_order() {
        let mut s = SqliteStorage::open_in_memory().unwrap();
        let c = swap("sigC", 0, 300, "W1", Side::Sell);
        let a = swap("sigA", 1, 100, "W1", Side::Buy);
        let b = swap("sigB", 0, 200, "W1", Side::Buy);
        let other = swap("sigX", 0, 250, "W2", Side::Buy);
        for e in [&c, &a, &b, &other] {
            s.insert_swap(e).unwrap();
        }
        let got = s.swaps_for_wallet("W1").unwrap();
        assert_eq!(got, vec![a.clone(), b.clone(), c.clone()]);
        let recent = s.recent_swaps(2).unwrap();
        assert_eq!(recent, vec![c, other]);
        assert_eq!(s.last_seen_slot("W1").unwrap(), Some(300));
        assert_eq!(s.last_seen_slot("W2").unwrap(), Some(250));
        assert_eq!(s.swaps_since(1_788_906_323).unwrap().len(), 4);
        assert_eq!(s.swaps_since(1_788_906_324).unwrap().len(), 0);
        assert_eq!(s.last_seen_slot("W3").unwrap(), None);
        assert_eq!(s.mint_decimals("MintAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(), Some(6));
    }

    #[test]
    fn unknown_and_not_swap_advance_cursor() {
        let mut s = SqliteStorage::open_in_memory().unwrap();
        let u = UnknownSwap {
            signature: "sigU".into(),
            slot: 500,
            block_time: None,
            wallet: "W1".into(),
            programs: vec!["JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4".into()],
        };
        assert_eq!(s.insert_unknown(&u).unwrap(), Inserted::New);
        assert_eq!(s.insert_unknown(&u).unwrap(), Inserted::Duplicate);
        assert_eq!(s.mark_not_swap("sigN", "W1", 600).unwrap(), Inserted::New);
        assert_eq!(s.mark_not_swap("sigN", "W1", 600).unwrap(), Inserted::Duplicate);
        assert_eq!(s.last_seen_slot("W1").unwrap(), Some(600));
        assert!(s.swaps_for_wallet("W1").unwrap().is_empty());
        assert_eq!(s.recent_unknown(10).unwrap(), vec![u]);
    }

    #[test]
    fn mint_decimals_upsert() {
        let mut s = SqliteStorage::open_in_memory().unwrap();
        assert_eq!(s.mint_decimals("M").unwrap(), None);
        s.put_mint_decimals("M", 6).unwrap();
        s.put_mint_decimals("M", 9).unwrap();
        assert_eq!(s.mint_decimals("M").unwrap(), Some(9));
    }

    #[test]
    fn token_meta_cache() {
        let mut s = SqliteStorage::open_in_memory().unwrap();
        s.insert_swap(&swap("sigA", 0, 1, "W1", Side::Buy)).unwrap();
        assert_eq!(
            s.mints_missing_meta().unwrap(),
            vec!["MintAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()]
        );
        let m = TokenMeta {
            mint: "MintAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            symbol: "AAA".into(),
            name: "Triple A".into(),
            uri: "https://example/meta.json".into(),
            image: String::new(),
        };
        s.put_token_meta(&m).unwrap();
        assert_eq!(s.token_meta(&m.mint).unwrap(), Some(m.clone()));
        assert!(s.mints_missing_meta().unwrap().is_empty());
        assert_eq!(s.mints_missing_image().unwrap(), vec![m.mint.clone()]);
        let mut m2 = m.clone();
        m2.symbol = "AAB".into();
        m2.image = "https://example/img.png".into();
        s.put_token_meta(&m2).unwrap(); // upsert
        assert_eq!(s.all_token_meta().unwrap(), vec![m2]);
        assert!(s.mints_missing_image().unwrap().is_empty());
    }

    #[test]
    fn every_venue_round_trips() {
        let mut s = SqliteStorage::open_in_memory().unwrap();
        for (i, v) in Venue::ALL.iter().enumerate() {
            let mut e = swap(&format!("sig{i}"), 0, i as u64, "W1", Side::Buy);
            e.venue = *v;
            assert_eq!(s.insert_swap(&e).unwrap(), Inserted::New, "{v:?}");
        }
        let got = s.swaps_for_wallet("W1").unwrap();
        assert_eq!(got.iter().map(|e| e.venue).collect::<Vec<_>>(), Venue::ALL.to_vec());
    }

    #[test]
    fn migration_is_idempotent_on_reopen() {
        let dir = std::env::temp_dir().join(format!("soltrack-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let mut s = SqliteStorage::open(&path).unwrap();
            s.insert_swap(&swap("sigA", 0, 1, "W1", Side::Buy)).unwrap();
        }
        let s = SqliteStorage::open(&path).unwrap();
        assert_eq!(s.swaps_for_wallet("W1").unwrap().len(), 1);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
