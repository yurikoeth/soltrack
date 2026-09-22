//! Versioned schema. Append a new entry to `MIGRATIONS` to change the schema;
//! never edit an existing one.

use rusqlite::Connection;

use crate::{Result, StoreError};

const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);

    -- Every signature we've fetched + classified, regardless of outcome.
    -- Drives `has_signature` and `last_seen_slot`.
    CREATE TABLE processed_signatures (
        signature   TEXT    PRIMARY KEY,
        wallet      TEXT    NOT NULL,
        slot        INTEGER NOT NULL,
        outcome     TEXT    NOT NULL CHECK (outcome IN ('swap','unknown','not_swap')),
        seen_at     INTEGER NOT NULL
    );
    CREATE INDEX ix_processed_wallet_slot ON processed_signatures (wallet, slot DESC);

    CREATE TABLE swaps (
        signature       TEXT    NOT NULL REFERENCES processed_signatures(signature),
        ix_index        INTEGER NOT NULL,
        slot            INTEGER NOT NULL,
        block_time      INTEGER,
        wallet          TEXT    NOT NULL,
        mint            TEXT    NOT NULL,
        venue           TEXT    NOT NULL CHECK (venue IN ('pumpfun_curve','pumpswap_amm')),
        side            TEXT    NOT NULL CHECK (side IN ('buy','sell')),
        token_amount    INTEGER NOT NULL,
        token_decimals  INTEGER NOT NULL,
        sol_amount      INTEGER NOT NULL,
        fee_lamports    INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (signature, ix_index)
    );
    CREATE INDEX ix_swaps_wallet_order ON swaps (wallet, slot, ix_index);
    CREATE INDEX ix_swaps_recent       ON swaps (slot DESC, ix_index DESC);
    CREATE INDEX ix_swaps_wallet_mint  ON swaps (wallet, mint);

    CREATE TABLE unknown_swaps (
        signature   TEXT    PRIMARY KEY REFERENCES processed_signatures(signature),
        slot        INTEGER NOT NULL,
        block_time  INTEGER,
        wallet      TEXT    NOT NULL,
        programs    TEXT    NOT NULL
    );

    CREATE TABLE mints (
        mint        TEXT    PRIMARY KEY,
        decimals    INTEGER NOT NULL
    );
    ",
    // v2: token display metadata cache
    "
    CREATE TABLE token_meta (
        mint        TEXT    PRIMARY KEY,
        symbol      TEXT    NOT NULL,
        name        TEXT    NOT NULL,
        uri         TEXT    NOT NULL DEFAULT '',
        fetched_at  INTEGER NOT NULL
    );
    ",
    // v3: more venues — drop the CHECK constraint on swaps.venue (SQLite
    // can't alter constraints in place, so rebuild the table).
    "
    CREATE TABLE swaps_v3 (
        signature       TEXT    NOT NULL REFERENCES processed_signatures(signature),
        ix_index        INTEGER NOT NULL,
        slot            INTEGER NOT NULL,
        block_time      INTEGER,
        wallet          TEXT    NOT NULL,
        mint            TEXT    NOT NULL,
        venue           TEXT    NOT NULL,
        side            TEXT    NOT NULL CHECK (side IN ('buy','sell')),
        token_amount    INTEGER NOT NULL,
        token_decimals  INTEGER NOT NULL,
        sol_amount      INTEGER NOT NULL,
        fee_lamports    INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (signature, ix_index)
    );
    INSERT INTO swaps_v3 SELECT signature, ix_index, slot, block_time, wallet, mint, venue, side,
                                token_amount, token_decimals, sol_amount, fee_lamports FROM swaps;
    DROP TABLE swaps;
    ALTER TABLE swaps_v3 RENAME TO swaps;
    CREATE INDEX ix_swaps_wallet_order ON swaps (wallet, slot, ix_index);
    CREATE INDEX ix_swaps_recent       ON swaps (slot DESC, ix_index DESC);
    CREATE INDEX ix_swaps_wallet_mint  ON swaps (wallet, mint);
    ",
    // v4: token image URL (from the metadata JSON at `uri`)
    "
    ALTER TABLE token_meta ADD COLUMN image TEXT NOT NULL DEFAULT '';
    ",
];

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")?;
    let current: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))?;
    let current = usize::try_from(current)
        .map_err(|_| StoreError::Migration(format!("bad version {current}")))?;
    if current > MIGRATIONS.len() {
        return Err(StoreError::Migration(format!(
            "database is at schema v{current} but this build only knows v{}",
            MIGRATIONS.len()
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let version = i + 1;
        tracing::info!(version, "applying store migration");
        conn.execute_batch("BEGIN;")?;
        if let Err(e) = conn.execute_batch(sql) {
            let _ = conn.execute_batch("ROLLBACK;");
            return Err(StoreError::Migration(format!("v{version}: {e}")));
        }
        conn.execute("INSERT INTO schema_version (version) VALUES (?1)", [version as i64])?;
        conn.execute_batch("COMMIT;")?;
    }
    Ok(())
}
