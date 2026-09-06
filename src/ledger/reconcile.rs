//! Reconciliation against observed balances.

use rusqlite::{params, Row};

use super::{
    account_by_name, balance_minor, load_entry, parse_account_amount, resolve_ts, write_entry,
    Ledger, NewEntry, Written,
};
use crate::error::Result;
use crate::model::{Kind, ReconcileResult, Snapshot, SnapshotList};
use crate::money::format_amount;
use crate::time;

#[derive(Clone, Debug)]
pub struct ReconcileRequest {
    pub account: String,
    pub observed: String,
    pub source: Option<String>,
    pub ts: Option<String>,
    pub adjust: bool,
    pub actor: Option<String>,
}

const SNAPSHOT_SELECT: &str =
    "SELECT s.id, a.name, a.decimals, s.ts, s.observed, s.book, s.diff, s.adjustment_entry_id, s.source \
     FROM snapshots s JOIN accounts a ON a.id = s.account_id";

fn snapshot_from_row(r: &Row<'_>) -> rusqlite::Result<Snapshot> {
    let d = r.get::<_, i64>(2)? as u32;
    Ok(Snapshot {
        id: r.get(0)?,
        account: r.get(1)?,
        ts: r.get(3)?,
        observed: format_amount(r.get(4)?, d),
        book: format_amount(r.get(5)?, d),
        diff: format_amount(r.get(6)?, d),
        adjustment_entry_id: r.get(7)?,
        source: r.get(8)?,
    })
}

impl Ledger {
    pub fn reconcile(&mut self, req: &ReconcileRequest) -> Result<ReconcileResult> {
        let tx = self.write_tx()?;
        let acc = account_by_name(&tx, &req.account)?;
        let observed = parse_account_amount(&req.observed, &acc)?;
        let ts = match req.ts.as_deref() {
            Some(t) => resolve_ts(Some(t))?,
            // An implicit reconcile must see the whole book, even entries stamped with a venue
            // time later than this machine's clock. Explicit --ts is the only way to go historical.
            None => {
                let now = time::now();
                let latest: Option<String> = tx.query_row(
                    "SELECT MAX(ts) FROM entries WHERE account_id = ?1",
                    params![acc.id],
                    |r| r.get(0),
                )?;
                match latest {
                    Some(l) if l > now => l,
                    _ => now,
                }
            }
        };
        let book = balance_minor(&tx, acc.id, Some(&ts))?;
        let diff = observed - book;

        // The adjustment goes in first so the snapshot can reference it: snapshots are append-only.
        let adjustment = if diff != 0 && req.adjust {
            let new = NewEntry {
                account: &acc,
                ts: ts.clone(),
                kind: Kind::Adjustment,
                amount: diff,
                reference: None,
                memo: Some(format!(
                    "reconcile: observed {}, book {}",
                    format_amount(observed, acc.decimals),
                    format_amount(book, acc.decimals)
                )),
                actor: req.actor.clone(),
                group_id: None,
                meta: None,
                reverses_id: None,
            };
            let Written::Inserted(id) = write_entry(&tx, &new)? else {
                unreachable!("adjustments carry no ref")
            };
            Some(load_entry(&tx, id)?)
        } else {
            None
        };

        tx.execute(
            "INSERT INTO snapshots (account_id, ts, observed, book, diff, adjustment_entry_id, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                acc.id,
                ts,
                observed,
                book,
                diff,
                adjustment.as_ref().map(|e| e.id),
                req.source.as_deref().map(str::trim)
            ],
        )?;
        let snapshot_id = tx.last_insert_rowid();
        let snapshot = tx.query_row(
            &format!("{SNAPSHOT_SELECT} WHERE s.id = ?1"),
            params![snapshot_id],
            snapshot_from_row,
        )?;
        tx.commit()?;
        Ok(ReconcileResult {
            snapshot,
            adjustment,
        })
    }

    pub fn snapshots(&self, account: &str, limit: usize) -> Result<SnapshotList> {
        let acc = account_by_name(&self.conn, account)?;
        let limit: i64 = if limit == 0 { -1 } else { limit as i64 };
        let mut stmt = self.conn.prepare(&format!(
            "{SNAPSHOT_SELECT} WHERE s.account_id = ?1 ORDER BY s.ts DESC, s.id DESC LIMIT ?2"
        ))?;
        let mut snapshots = stmt
            .query_map(params![acc.id, limit], snapshot_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        snapshots.reverse();
        Ok(SnapshotList {
            account: acc.name,
            snapshots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::entries::tests::{ledger_with, req};
    use crate::ledger::AddRequest;

    fn rreq(observed: &str) -> ReconcileRequest {
        ReconcileRequest {
            account: "w".into(),
            observed: observed.into(),
            source: Some("chain".into()),
            ts: None,
            adjust: true,
            actor: Some("claude".into()),
        }
    }

    #[test]
    fn posts_adjustment_so_book_matches_observed() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&req("w", "126.2", Kind::Deposit)).unwrap();
        let r = l.reconcile(&rreq("124.70")).unwrap();
        assert_eq!(r.snapshot.observed, "124.700000");
        assert_eq!(r.snapshot.book, "126.200000");
        assert_eq!(r.snapshot.diff, "-1.500000");
        assert_eq!(r.snapshot.source.as_deref(), Some("chain"));
        let adj = r.adjustment.unwrap();
        assert_eq!(adj.kind, Kind::Adjustment);
        assert_eq!(adj.amount, "-1.500000");
        assert_eq!(adj.ts, r.snapshot.ts);
        assert_eq!(
            adj.memo.as_deref(),
            Some("reconcile: observed 124.700000, book 126.200000")
        );
        assert_eq!(r.snapshot.adjustment_entry_id, Some(adj.id));
        assert_eq!(l.balance("w", None).unwrap().balance, "124.700000");
        assert_eq!(
            l.balances().unwrap()[0].last_reconciled_at.as_deref(),
            Some(r.snapshot.ts.as_str())
        );
    }

    #[test]
    fn zero_diff_and_no_adjust_write_no_entry() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&req("w", "10", Kind::Deposit)).unwrap();
        let same = l.reconcile(&rreq("10")).unwrap();
        assert!(same.adjustment.is_none());
        assert_eq!(same.snapshot.diff, "0.000000");
        let skipped = l
            .reconcile(&ReconcileRequest {
                adjust: false,
                ..rreq("12")
            })
            .unwrap();
        assert!(skipped.adjustment.is_none());
        assert_eq!(skipped.snapshot.diff, "2.000000");
        assert_eq!(l.balance("w", None).unwrap().balance, "10.000000");
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 2);
    }

    #[test]
    fn ts_computes_book_as_of_that_time() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        l.add(&AddRequest {
            ts: Some("2026-09-01".into()),
            ..req("w", "10", Kind::Deposit)
        })
        .unwrap();
        l.add(&AddRequest {
            ts: Some("2026-09-05".into()),
            ..req("w", "-3", Kind::Trade)
        })
        .unwrap();
        let r = l
            .reconcile(&ReconcileRequest {
                ts: Some("2026-09-02".into()),
                ..rreq("9")
            })
            .unwrap();
        assert_eq!(r.snapshot.book, "10.00");
        assert_eq!(r.snapshot.diff, "-1.00");
        assert_eq!(r.snapshot.ts, "2026-09-02T00:00:00.000Z");
        assert_eq!(l.balance("w", Some("2026-09-02")).unwrap().balance, "9.00");
        assert_eq!(l.balance("w", None).unwrap().balance, "6.00");
    }

    #[test]
    fn default_ts_never_predates_booked_entries() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        l.add(&AddRequest {
            ts: Some("2999-01-01T00:00:00Z".into()),
            ..req("w", "10", Kind::Deposit)
        })
        .unwrap();
        let r = l.reconcile(&rreq("10")).unwrap();
        assert_eq!(r.snapshot.book, "10.00");
        assert_eq!(r.snapshot.diff, "0.00");
        assert_eq!(r.snapshot.ts, "2999-01-01T00:00:00.000Z");
        assert!(r.adjustment.is_none());
    }

    #[test]
    fn observed_zero_is_allowed_and_snapshots_list_oldest_first_with_limit() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        l.add(&AddRequest {
            ts: Some("2026-08-31".into()),
            ..req("w", "5", Kind::Deposit)
        })
        .unwrap();
        // 09-01 sees book 5 and posts -5; the later two see book 0 and post nothing.
        l.reconcile(&ReconcileRequest {
            ts: Some("2026-09-01".into()),
            ..rreq("0")
        })
        .unwrap();
        l.reconcile(&ReconcileRequest {
            ts: Some("2026-09-02".into()),
            ..rreq("0")
        })
        .unwrap();
        l.reconcile(&ReconcileRequest {
            ts: Some("2026-09-03".into()),
            ..rreq("0")
        })
        .unwrap();
        let s = l.snapshots("w", 2).unwrap();
        assert_eq!(s.snapshots.len(), 2);
        assert_eq!(s.snapshots[0].ts, "2026-09-02T00:00:00.000Z");
        assert_eq!(s.snapshots[1].ts, "2026-09-03T00:00:00.000Z");
        assert_eq!(l.balance("w", None).unwrap().balance, "0.00");
    }
}
