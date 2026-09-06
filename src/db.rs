//! SQLite connection setup and schema migrations.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::Result;

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = r#"
CREATE TABLE accounts (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
  currency    TEXT NOT NULL,
  decimals    INTEGER NOT NULL CHECK (decimals BETWEEN 0 AND 18),
  note        TEXT,
  created_at  TEXT NOT NULL
);

CREATE TABLE entries (
  id           INTEGER PRIMARY KEY,
  account_id   INTEGER NOT NULL REFERENCES accounts(id),
  ts           TEXT NOT NULL,
  recorded_at  TEXT NOT NULL,
  kind         TEXT NOT NULL CHECK (kind IN
                 ('deposit','withdrawal','trade','settlement','fee',
                  'transfer','adjustment','reversal','other')),
  amount       INTEGER NOT NULL CHECK (amount <> 0),
  ref          TEXT,
  memo         TEXT,
  actor        TEXT,
  group_id     TEXT,
  meta         TEXT CHECK (meta IS NULL OR json_type(meta) = 'object'),
  reverses_id  INTEGER REFERENCES entries(id)
);
CREATE UNIQUE INDEX entries_account_ref ON entries(account_id, ref) WHERE ref IS NOT NULL;
CREATE UNIQUE INDEX entries_reverses    ON entries(reverses_id)     WHERE reverses_id IS NOT NULL;
CREATE INDEX        entries_account_ts  ON entries(account_id, ts, id);
CREATE INDEX        entries_group       ON entries(group_id)        WHERE group_id IS NOT NULL;

CREATE TRIGGER entries_no_update BEFORE UPDATE ON entries
  BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;
CREATE TRIGGER entries_no_delete BEFORE DELETE ON entries
  BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;

CREATE TABLE snapshots (
  id                   INTEGER PRIMARY KEY,
  account_id           INTEGER NOT NULL REFERENCES accounts(id),
  ts                   TEXT NOT NULL,
  observed             INTEGER NOT NULL,
  book                 INTEGER NOT NULL,
  diff                 INTEGER NOT NULL,
  adjustment_entry_id  INTEGER REFERENCES entries(id),
  source               TEXT
);
CREATE TRIGGER snapshots_no_update BEFORE UPDATE ON snapshots
  BEGIN SELECT RAISE(ABORT, 'ledger snapshots are append-only'); END;
CREATE TRIGGER snapshots_no_delete BEFORE DELETE ON snapshots
  BEGIN SELECT RAISE(ABORT, 'ledger snapshots are append-only'); END;
"#;

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    let version: i64 = conn.query_row(
        "SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'), 0)",
        [],
        |r| r.get(0),
    )?;
    if version < 1 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V1} INSERT INTO meta (key, value) VALUES ('schema_version', '1'); COMMIT;"
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = open_in_memory().unwrap();
        c.execute_batch(
            "INSERT INTO accounts (name, currency, decimals, created_at) VALUES ('a', 'USD', 2, 't');
             INSERT INTO accounts (name, currency, decimals, created_at) VALUES ('b', 'USD', 2, 't');
             INSERT INTO entries (account_id, ts, recorded_at, kind, amount, ref) VALUES (1, 't', 't', 'deposit', 100, 'r1');
             INSERT INTO snapshots (account_id, ts, observed, book, diff) VALUES (1, 't', 100, 100, 0);",
        )
        .unwrap();
        c
    }

    #[test]
    fn creates_schema_and_records_version() {
        let c = open_in_memory().unwrap();
        let v: String = c
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('accounts','entries','snapshots')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn open_creates_parent_dir_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("ledger.db");
        open(&p).unwrap();
        let c = open(&p).unwrap();
        assert!(p.exists());
        let mode: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let fk: i64 = c
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn entries_are_append_only() {
        let c = seeded();
        let up = c.execute("UPDATE entries SET amount = 1", []).unwrap_err();
        assert!(up.to_string().contains("append-only"), "{up}");
        let del = c.execute("DELETE FROM entries", []).unwrap_err();
        assert!(del.to_string().contains("append-only"), "{del}");
    }

    #[test]
    fn snapshots_are_append_only() {
        let c = seeded();
        assert!(c
            .execute("UPDATE snapshots SET diff = 1", [])
            .unwrap_err()
            .to_string()
            .contains("append-only"));
        assert!(c
            .execute("DELETE FROM snapshots", [])
            .unwrap_err()
            .to_string()
            .contains("append-only"));
    }

    #[test]
    fn meta_must_be_a_json_object() {
        let c = seeded();
        let bad = c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, meta) VALUES (1,'t','t','trade',-1,'[1]')",
            [],
        );
        assert!(bad.is_err());
        c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, meta) VALUES (1,'t','t','trade',-1,'{\"a\":1}')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn ref_is_unique_per_account_only() {
        let c = seeded();
        let dup = c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, ref) VALUES (1,'t','t','deposit',5,'r1')",
            [],
        );
        assert!(dup.is_err());
        c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, ref) VALUES (2,'t','t','deposit',5,'r1')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn zero_amount_and_bad_kind_rejected_by_schema() {
        let c = seeded();
        assert!(c
            .execute(
                "INSERT INTO entries (account_id, ts, recorded_at, kind, amount) VALUES (1,'t','t','trade',0)",
                []
            )
            .is_err());
        assert!(c
            .execute(
                "INSERT INTO entries (account_id, ts, recorded_at, kind, amount) VALUES (1,'t','t','pnl',1)",
                []
            )
            .is_err());
    }
}
