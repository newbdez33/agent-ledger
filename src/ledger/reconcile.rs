//! Reconciliation against observed balances.

use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{
    account_by_name, balance_minor, load_entry, parse_account_amount, resolve_ts, write_entry,
    AccountRow, Ledger, NewEntry, Written,
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
    /// Post an `adjustment` for a nonzero diff. Off by default: in a live loop a diff is usually
    /// activity that has not been booked yet, not a discrepancy.
    pub adjust: bool,
    /// Compute observed, book and diff and write nothing.
    pub dry_run: bool,
    /// Why the adjustment is right; appended to its generated memo. Only used with `adjust`.
    pub memo: Option<String>,
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

/// One observation compared with the book as of its time.
struct Comparison {
    acc: AccountRow,
    ts: String,
    observed: i64,
    book: i64,
    diff: i64,
    source: Option<String>,
}

fn compare(conn: &Connection, req: &ReconcileRequest) -> Result<Comparison> {
    let acc = account_by_name(conn, &req.account)?;
    let observed = parse_account_amount(&req.observed, &acc)?;
    let ts = match req.ts.as_deref() {
        Some(t) => resolve_ts(Some(t))?,
        // An implicit reconcile must see the whole book, even entries stamped with a venue
        // time later than this machine's clock. Explicit --ts is the only way to go historical.
        None => {
            let now = time::now();
            let latest: Option<String> = conn.query_row(
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
    let book = balance_minor(conn, acc.id, Some(&ts))?;
    Ok(Comparison {
        diff: observed - book,
        acc,
        ts,
        observed,
        book,
        source: req.source.as_deref().map(|s| s.trim().to_string()),
    })
}

/// The snapshot that already records this exact observation, if any. A snapshot is the fact
/// "source S observed X at T against book B"; the same fact is never stored twice.
fn existing_snapshot(conn: &Connection, c: &Comparison) -> Result<Option<Snapshot>> {
    Ok(conn
        .query_row(
            &format!(
                "{SNAPSHOT_SELECT} WHERE s.account_id = ?1 AND s.ts = ?2 AND s.observed = ?3 \
                 AND s.book = ?4 AND s.source IS ?5 ORDER BY s.id DESC LIMIT 1"
            ),
            params![c.acc.id, c.ts, c.observed, c.book, c.source],
            snapshot_from_row,
        )
        .optional()?)
}

impl Ledger {
    pub fn reconcile(&mut self, req: &ReconcileRequest) -> Result<ReconcileResult> {
        if req.dry_run {
            let c = compare(&self.conn, req)?;
            let duplicate = existing_snapshot(&self.conn, &c)?.is_some();
            let d = c.acc.decimals;
            return Ok(ReconcileResult {
                snapshot: Snapshot {
                    id: None,
                    account: c.acc.name,
                    ts: c.ts,
                    observed: format_amount(c.observed, d),
                    book: format_amount(c.book, d),
                    diff: format_amount(c.diff, d),
                    adjustment_entry_id: None,
                    source: c.source,
                },
                adjustment: None,
                duplicate,
                dry_run: true,
            });
        }

        let tx = self.write_tx()?;
        let c = compare(&tx, req)?;
        let posts_adjustment = c.diff != 0 && req.adjust;
        if !posts_adjustment {
            if let Some(existing) = existing_snapshot(&tx, &c)? {
                let adjustment = match existing.adjustment_entry_id {
                    Some(id) => Some(load_entry(&tx, id)?),
                    None => None,
                };
                return Ok(ReconcileResult {
                    snapshot: existing,
                    adjustment,
                    duplicate: true,
                    dry_run: false,
                });
            }
        }

        // The adjustment goes in first so the snapshot can reference it: snapshots are append-only.
        let adjustment = if posts_adjustment {
            let new = NewEntry {
                account: &c.acc,
                ts: c.ts.clone(),
                kind: Kind::Adjustment,
                amount: c.diff,
                reference: None,
                memo: Some(format!(
                    "reconcile: observed {}, book {}{}",
                    format_amount(c.observed, c.acc.decimals),
                    format_amount(c.book, c.acc.decimals),
                    req.memo
                        .as_deref()
                        .map(str::trim)
                        .filter(|m| !m.is_empty())
                        .map(|m| format!("; {m}"))
                        .unwrap_or_default()
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
                c.acc.id,
                c.ts,
                c.observed,
                c.book,
                c.diff,
                adjustment.as_ref().map(|e| e.id),
                c.source
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
            duplicate: false,
            dry_run: false,
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
            adjust: false,
            dry_run: false,
            memo: None,
            actor: Some("claude".into()),
        }
    }

    fn adjusting(observed: &str) -> ReconcileRequest {
        ReconcileRequest {
            adjust: true,
            ..rreq(observed)
        }
    }

    #[test]
    fn posts_adjustment_so_book_matches_observed() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&req("w", "126.2", Kind::Deposit)).unwrap();
        let r = l.reconcile(&adjusting("124.70")).unwrap();
        assert!(!r.duplicate && !r.dry_run);
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
    fn zero_diff_and_the_default_write_no_entry() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&req("w", "10", Kind::Deposit)).unwrap();
        let same = l.reconcile(&adjusting("10")).unwrap();
        assert!(same.adjustment.is_none());
        assert_eq!(same.snapshot.diff, "0.000000");
        // Without --adjust a nonzero diff is recorded on the snapshot and nothing else happens.
        let observed = l.reconcile(&rreq("12")).unwrap();
        assert!(observed.adjustment.is_none());
        assert_eq!(observed.snapshot.diff, "2.000000");
        assert!(observed.snapshot.adjustment_entry_id.is_none());
        assert_eq!(l.balance("w", None).unwrap().balance, "10.000000");
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 2);
    }

    #[test]
    fn adjustment_memo_appends_the_callers_reason() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&AddRequest {
            ts: Some("2026-09-10T09:00:00Z".into()),
            ..req("w", "128.57", Kind::Deposit)
        })
        .unwrap();
        let r = l
            .reconcile(&ReconcileRequest {
                ts: Some("2026-09-10T10:00:00Z".into()),
                memo: Some("support: on-chain fee, no activity row".into()),
                ..adjusting("128")
            })
            .unwrap();
        assert_eq!(
            r.adjustment.unwrap().memo.as_deref(),
            Some("reconcile: observed 128.000000, book 128.570000; support: on-chain fee, no activity row")
        );
    }

    #[test]
    fn dry_run_reports_the_diff_and_writes_nothing() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&AddRequest {
            ts: Some("2026-09-08T09:00:00Z".into()),
            ..req("w", "10", Kind::Deposit)
        })
        .unwrap();
        let r = l
            .reconcile(&ReconcileRequest {
                dry_run: true,
                adjust: true,
                ts: Some("2026-09-08T10:00:00Z".into()),
                ..rreq("12.5")
            })
            .unwrap();
        assert!(r.dry_run);
        assert!(!r.duplicate);
        assert_eq!(r.snapshot.id, None);
        assert_eq!(r.snapshot.ts, "2026-09-08T10:00:00.000Z");
        assert_eq!(r.snapshot.observed, "12.500000");
        assert_eq!(r.snapshot.book, "10.000000");
        assert_eq!(r.snapshot.diff, "2.500000");
        assert_eq!(r.snapshot.source.as_deref(), Some("chain"));
        assert!(r.adjustment.is_none());
        assert!(l.snapshots("w", 0).unwrap().snapshots.is_empty());
        assert_eq!(l.balance("w", None).unwrap().balance, "10.000000");
        assert_eq!(l.balances().unwrap()[0].entries, 1);
    }

    #[test]
    fn same_observation_at_the_same_time_is_a_duplicate() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&AddRequest {
            ts: Some("2026-09-08T09:00:00Z".into()),
            ..req("w", "10", Kind::Deposit)
        })
        .unwrap();
        let at = || ReconcileRequest {
            ts: Some("2026-09-08T10:00:00Z".into()),
            ..rreq("9.5")
        };
        let first = l.reconcile(&at()).unwrap();
        assert!(!first.duplicate);
        let again = l.reconcile(&at()).unwrap();
        assert!(again.duplicate);
        assert_eq!(again.snapshot.id, first.snapshot.id);
        assert_eq!(again.snapshot.diff, "-0.500000");
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 1);
        // A dry run of the same observation says so without writing.
        let peek = l
            .reconcile(&ReconcileRequest {
                dry_run: true,
                ..at()
            })
            .unwrap();
        assert!(peek.dry_run && peek.duplicate);
        assert_eq!(peek.snapshot.id, None);
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 1);
    }

    #[test]
    fn observe_then_adjust_at_the_same_time_is_not_a_duplicate() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&AddRequest {
            ts: Some("2026-09-08T09:00:00Z".into()),
            ..req("w", "10", Kind::Deposit)
        })
        .unwrap();
        let ts = Some("2026-09-08T10:00:00Z".to_string());
        let looked = l
            .reconcile(&ReconcileRequest {
                ts: ts.clone(),
                ..rreq("9.5")
            })
            .unwrap();
        assert!(looked.adjustment.is_none());
        let fixed = l
            .reconcile(&ReconcileRequest {
                ts: ts.clone(),
                ..adjusting("9.5")
            })
            .unwrap();
        assert!(!fixed.duplicate);
        let adj = fixed.adjustment.expect("adjustment posted");
        assert_eq!(adj.amount, "-0.500000");
        assert_eq!(fixed.snapshot.adjustment_entry_id, Some(adj.id));
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 2);
        assert_eq!(l.balance("w", None).unwrap().balance, "9.500000");
        // Replaying the adjusting call: the book now equals observed, so it is a fresh
        // zero-diff snapshot, and replaying that one is a duplicate.
        let replay = l
            .reconcile(&ReconcileRequest {
                ts: ts.clone(),
                ..adjusting("9.5")
            })
            .unwrap();
        assert!(!replay.duplicate);
        assert_eq!(replay.snapshot.diff, "0.000000");
        assert!(replay.adjustment.is_none());
        let replay2 = l
            .reconcile(&ReconcileRequest {
                ts,
                ..adjusting("9.5")
            })
            .unwrap();
        assert!(replay2.duplicate);
        assert_eq!(replay2.snapshot.id, replay.snapshot.id);
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 3);
    }

    #[test]
    fn duplicate_needs_the_same_book_and_source() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        l.add(&AddRequest {
            ts: Some("2026-09-01".into()),
            ..req("w", "10", Kind::Deposit)
        })
        .unwrap();
        let at = |source: Option<&str>| ReconcileRequest {
            ts: Some("2026-09-08T10:00:00Z".into()),
            source: source.map(str::to_string),
            ..rreq("10")
        };
        l.reconcile(&at(None)).unwrap();
        assert!(
            l.reconcile(&at(None)).unwrap().duplicate,
            "null source matches null"
        );
        assert!(!l.reconcile(&at(Some("chain"))).unwrap().duplicate);
        // A back-dated entry changes the book as of that time, so the same observation is new.
        l.add(&AddRequest {
            ts: Some("2026-09-02".into()),
            ..req("w", "-1", Kind::Fee)
        })
        .unwrap();
        let r = l.reconcile(&at(None)).unwrap();
        assert!(!r.duplicate);
        assert_eq!(r.snapshot.book, "9.00");
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 3);
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
                ..adjusting("9")
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
            ..adjusting("0")
        })
        .unwrap();
        l.reconcile(&ReconcileRequest {
            ts: Some("2026-09-02".into()),
            ..adjusting("0")
        })
        .unwrap();
        l.reconcile(&ReconcileRequest {
            ts: Some("2026-09-03".into()),
            ..adjusting("0")
        })
        .unwrap();
        let s = l.snapshots("w", 2).unwrap();
        assert_eq!(s.snapshots.len(), 2);
        assert_eq!(s.snapshots[0].ts, "2026-09-02T00:00:00.000Z");
        assert_eq!(s.snapshots[1].ts, "2026-09-03T00:00:00.000Z");
        assert_eq!(l.balance("w", None).unwrap().balance, "0.00");
    }
}
