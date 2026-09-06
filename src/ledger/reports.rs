//! Reading the books: balances, history, export, group view.

use std::collections::BTreeMap;

use rusqlite::params;

use super::{
    account_by_name, balance_minor, entry_from_row, normalize_opt_ts, validate_group, Ledger,
    ENTRY_SELECT,
};
use crate::error::{LedgerError, Result};
use crate::model::{AccountBalance, BalanceAt, Entry, GroupView, History, HistoryEntry, Kind};
use crate::money::{format_amount, format_wide};

#[derive(Clone, Debug, Default)]
pub struct HistoryFilter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub kind: Option<Kind>,
    pub group: Option<String>,
    /// 0 means unlimited.
    pub limit: usize,
}

impl Ledger {
    pub fn balances(&self) -> Result<Vec<AccountBalance>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.name, a.currency, a.decimals, COALESCE(SUM(e.amount), 0), COUNT(e.id), MAX(e.ts), \
                    (SELECT MAX(s.ts) FROM snapshots s WHERE s.account_id = a.id) \
             FROM accounts a LEFT JOIN entries e ON e.account_id = a.id \
             GROUP BY a.id ORDER BY a.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            let decimals = r.get::<_, i64>(2)? as u32;
            Ok(AccountBalance {
                account: r.get(0)?,
                currency: r.get(1)?,
                balance: format_amount(r.get(3)?, decimals),
                entries: r.get(4)?,
                last_ts: r.get(5)?,
                last_reconciled_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn balance(&self, account: &str, at: Option<&str>) -> Result<BalanceAt> {
        let acc = account_by_name(&self.conn, account)?;
        let at = normalize_opt_ts(at)?;
        let minor = balance_minor(&self.conn, acc.id, at.as_deref())?;
        Ok(BalanceAt {
            account: acc.name,
            currency: acc.currency,
            balance: format_amount(minor, acc.decimals),
            at,
        })
    }

    pub fn history(&self, account: &str, filter: &HistoryFilter) -> Result<History> {
        let acc = account_by_name(&self.conn, account)?;
        let since = normalize_opt_ts(filter.since.as_deref())?;
        let until = normalize_opt_ts(filter.until.as_deref())?;
        let group = validate_group(filter.group.as_deref())?;
        let limit: i64 = if filter.limit == 0 {
            -1
        } else {
            filter.limit as i64
        };
        let sql = "WITH running AS ( \
                     SELECT e.*, SUM(e.amount) OVER (ORDER BY e.ts, e.id) AS balance_after \
                     FROM entries e WHERE e.account_id = ?1) \
                   SELECT e.id, a.name, a.currency, a.decimals, e.ts, e.recorded_at, e.kind, e.amount, \
                          e.ref, e.memo, e.actor, e.group_id, e.meta, e.reverses_id, \
                          (SELECT r.id FROM entries r WHERE r.reverses_id = e.id), e.balance_after \
                   FROM running e JOIN accounts a ON a.id = e.account_id \
                   WHERE (?2 IS NULL OR e.ts >= ?2) AND (?3 IS NULL OR e.ts <= ?3) \
                     AND (?4 IS NULL OR e.kind = ?4) AND (?5 IS NULL OR e.group_id = ?5) \
                   ORDER BY e.ts DESC, e.id DESC LIMIT ?6";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(
            params![
                acc.id,
                since,
                until,
                filter.kind.map(Kind::as_str),
                group,
                limit
            ],
            |r| {
                let entry = entry_from_row(r)?;
                let balance_after = format_amount(r.get::<_, i64>(15)?, entry.decimals);
                Ok(HistoryEntry {
                    entry,
                    balance_after,
                })
            },
        )?;
        let mut entries = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        entries.reverse();
        Ok(History {
            account: acc.name,
            currency: acc.currency,
            entries,
        })
    }

    pub fn export(&self, account: &str) -> Result<History> {
        self.history(account, &HistoryFilter::default())
    }

    pub fn group(&self, id: &str) -> Result<GroupView> {
        let group = validate_group(Some(id))?.expect("Some in, Some out");
        let mut stmt = self.conn.prepare(&format!(
            "{ENTRY_SELECT} WHERE e.group_id = ?1 ORDER BY e.ts, e.id"
        ))?;
        let entries = stmt
            .query_map(params![&group], entry_from_row)?
            .collect::<rusqlite::Result<Vec<Entry>>>()?;
        if entries.is_empty() {
            return Err(LedgerError::GroupNotFound(group));
        }
        let mut max_decimals: BTreeMap<String, u32> = BTreeMap::new();
        for e in &entries {
            let d = max_decimals.entry(e.currency.clone()).or_insert(0);
            *d = (*d).max(e.decimals);
        }
        let mut sums: BTreeMap<String, i128> = BTreeMap::new();
        for e in &entries {
            let target = max_decimals[&e.currency];
            let scaled = e.amount_minor as i128 * 10i128.pow(target - e.decimals);
            *sums.entry(e.currency.clone()).or_insert(0) += scaled;
        }
        let net = sums
            .into_iter()
            .map(|(ccy, sum)| {
                let d = max_decimals[&ccy];
                (ccy, format_wide(sum, d))
            })
            .collect();
        Ok(GroupView {
            group,
            entries,
            net,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::entries::tests::{ledger_with, req};
    use crate::ledger::AddRequest;

    fn at(ts: &str, r: AddRequest) -> AddRequest {
        AddRequest {
            ts: Some(ts.into()),
            ..r
        }
    }

    #[test]
    fn balances_lists_every_account_with_counts() {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USDC", 6)]);
        l.add(&at("2026-09-01", req("a", "10", Kind::Deposit)))
            .unwrap();
        l.add(&at("2026-09-02", req("a", "-4", Kind::Trade)))
            .unwrap();
        let all = l.balances().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].account, "a");
        assert_eq!(all[0].balance, "6.00");
        assert_eq!(all[0].entries, 2);
        assert_eq!(all[0].last_ts.as_deref(), Some("2026-09-02T00:00:00.000Z"));
        assert_eq!(all[0].last_reconciled_at, None);
        assert_eq!(all[1].balance, "0.000000");
        assert_eq!(all[1].entries, 0);
    }

    #[test]
    fn balance_at_uses_movement_time() {
        let mut l = ledger_with(&[("a", "USD", 2)]);
        l.add(&at("2026-09-01T00:00:00Z", req("a", "10", Kind::Deposit)))
            .unwrap();
        l.add(&at("2026-09-03T00:00:00Z", req("a", "-4", Kind::Trade)))
            .unwrap();
        let b = l.balance("a", Some("2026-09-02")).unwrap();
        assert_eq!(b.balance, "10.00");
        assert_eq!(b.at.as_deref(), Some("2026-09-02T00:00:00.000Z"));
        assert_eq!(l.balance("a", None).unwrap().balance, "6.00");
        assert!(matches!(
            l.balance("a", Some("bad")),
            Err(LedgerError::InvalidTimestamp(_))
        ));
    }

    #[test]
    fn history_has_true_running_balance_under_filters() {
        let mut l = ledger_with(&[("a", "USD", 2)]);
        l.add(&at("2026-09-01", req("a", "10", Kind::Deposit)))
            .unwrap();
        l.add(&at(
            "2026-09-02",
            AddRequest {
                group: Some("g".into()),
                ..req("a", "-4", Kind::Trade)
            },
        ))
        .unwrap();
        l.add(&at("2026-09-03", req("a", "-1", Kind::Fee))).unwrap();
        l.add(&at("2026-09-04", req("a", "3", Kind::Settlement)))
            .unwrap();

        let full = l.history("a", &HistoryFilter::default()).unwrap();
        let after: Vec<&str> = full
            .entries
            .iter()
            .map(|e| e.balance_after.as_str())
            .collect();
        assert_eq!(after, ["10.00", "6.00", "5.00", "8.00"]);

        let last_two = l
            .history(
                "a",
                &HistoryFilter {
                    limit: 2,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(last_two.entries.len(), 2);
        assert_eq!(last_two.entries[0].balance_after, "5.00");
        assert_eq!(last_two.entries[1].balance_after, "8.00");

        let since = l
            .history(
                "a",
                &HistoryFilter {
                    since: Some("2026-09-03".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(since.entries.len(), 2);
        assert_eq!(since.entries[0].balance_after, "5.00");

        let fees = l
            .history(
                "a",
                &HistoryFilter {
                    kind: Some(Kind::Fee),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(fees.entries.len(), 1);
        assert_eq!(fees.entries[0].balance_after, "5.00");

        let grouped = l
            .history(
                "a",
                &HistoryFilter {
                    group: Some("g".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(grouped.entries.len(), 1);
        assert_eq!(grouped.entries[0].entry.amount, "-4.00");

        let until = l
            .history(
                "a",
                &HistoryFilter {
                    until: Some("2026-09-02".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(until.entries.len(), 2);
    }

    #[test]
    fn group_view_spans_accounts_and_nets_per_currency() {
        let mut l = ledger_with(&[
            ("poly", "USDC", 6),
            ("kalshi", "USD", 2),
            ("poly2", "USDC", 2),
        ]);
        let g = Some("arb:1".to_string());
        l.add(&AddRequest {
            group: g.clone(),
            ..req("poly", "-45", Kind::Trade)
        })
        .unwrap();
        l.add(&AddRequest {
            group: g.clone(),
            ..req("kalshi", "-52", Kind::Trade)
        })
        .unwrap();
        l.add(&AddRequest {
            group: g.clone(),
            ..req("poly", "100", Kind::Settlement)
        })
        .unwrap();
        l.add(&AddRequest {
            group: g.clone(),
            ..req("poly2", "0.5", Kind::Other)
        })
        .unwrap();
        let v = l.group("arb:1").unwrap();
        assert_eq!(v.entries.len(), 4);
        assert_eq!(v.net["USDC"], "55.500000");
        assert_eq!(v.net["USD"], "-52.00");
        assert!(matches!(
            l.group("nope"),
            Err(LedgerError::GroupNotFound(_))
        ));

        l.reverse_group("arb:1", None, None).unwrap();
        let after = l.group("arb:1").unwrap();
        assert_eq!(after.entries.len(), 8);
        assert_eq!(after.net["USDC"], "0.000000");
        assert_eq!(after.net["USD"], "0.00");
    }
}
