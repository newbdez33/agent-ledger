//! Realized cash PnL, excluding capital movements.

use rusqlite::types::Value;

use super::{account_by_name, all_accounts, normalize_opt_ts, AccountRow, Ledger};
use crate::error::{LedgerError, Result};
use crate::model::{AccountPnl, PnlRow};
use crate::money::format_amount;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PnlBucket {
    Total,
    Day,
    Week,
    Month,
    Group,
    Meta(String),
}

impl PnlBucket {
    pub fn parse(text: &str) -> Result<PnlBucket> {
        let t = text.trim();
        match t {
            "total" => Ok(PnlBucket::Total),
            "day" => Ok(PnlBucket::Day),
            "week" => Ok(PnlBucket::Week),
            "month" => Ok(PnlBucket::Month),
            "group" => Ok(PnlBucket::Group),
            _ => match t.strip_prefix("meta:") {
                Some(key)
                    if !key.is_empty()
                        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
                {
                    Ok(PnlBucket::Meta(key.to_string()))
                }
                _ => Err(LedgerError::InvalidBucket(t.to_string())),
            },
        }
    }

    fn sql_expr(&self) -> &'static str {
        match self {
            PnlBucket::Total => "NULL",
            PnlBucket::Day => "substr(x.ts, 1, 10)",
            PnlBucket::Week => "strftime('%Y-W%W', x.ts)",
            PnlBucket::Month => "substr(x.ts, 1, 7)",
            PnlBucket::Group => "x.group_id",
            PnlBucket::Meta(_) => "CAST(json_extract(x.meta, ?4) AS TEXT)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PnlFilter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub by: PnlBucket,
}

impl Ledger {
    pub fn pnl(&self, account: Option<&str>, filter: &PnlFilter) -> Result<Vec<AccountPnl>> {
        let accounts: Vec<AccountRow> = match account {
            Some(name) => vec![account_by_name(&self.conn, name)?],
            None => all_accounts(&self.conn)?,
        };
        let since = normalize_opt_ts(filter.since.as_deref())?;
        let until = normalize_opt_ts(filter.until.as_deref())?;
        let sql = format!(
            "SELECT {bucket} AS bucket, \
                    COALESCE(SUM(CASE WHEN x.k = 'trade' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'settlement' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'fee' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'adjustment' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'other' THEN x.amount END), 0), \
                    COALESCE(SUM(x.amount), 0) \
             FROM (SELECT e.ts, e.amount, e.group_id, e.meta, COALESCE(o.kind, e.kind) AS k \
                   FROM entries e LEFT JOIN entries o ON o.id = e.reverses_id \
                   WHERE e.account_id = ?1) x \
             WHERE x.k NOT IN ('deposit', 'withdrawal', 'transfer') \
               AND (?2 IS NULL OR x.ts >= ?2) AND (?3 IS NULL OR x.ts <= ?3) \
             GROUP BY bucket ORDER BY bucket",
            bucket = filter.by.sql_expr()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut out = Vec::with_capacity(accounts.len());
        for acc in accounts {
            let mut values: Vec<Value> = vec![
                Value::Integer(acc.id),
                since.clone().map_or(Value::Null, Value::Text),
                until.clone().map_or(Value::Null, Value::Text),
            ];
            if let PnlBucket::Meta(key) = &filter.by {
                values.push(Value::Text(format!("$.{key}")));
            }
            let d = acc.decimals;
            let rows = stmt.query_map(rusqlite::params_from_iter(values), |r| {
                Ok(PnlRow {
                    bucket: r.get(0)?,
                    trades: format_amount(r.get(1)?, d),
                    settlements: format_amount(r.get(2)?, d),
                    fees: format_amount(r.get(3)?, d),
                    adjustments: format_amount(r.get(4)?, d),
                    other: format_amount(r.get(5)?, d),
                    net: format_amount(r.get(6)?, d),
                })
            })?;
            let mut rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            if rows.is_empty() && filter.by == PnlBucket::Total {
                let zero = format_amount(0, d);
                rows.push(PnlRow {
                    bucket: None,
                    trades: zero.clone(),
                    settlements: zero.clone(),
                    fees: zero.clone(),
                    adjustments: zero.clone(),
                    other: zero.clone(),
                    net: zero,
                });
            }
            out.push(AccountPnl {
                account: acc.name,
                currency: acc.currency,
                rows,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::entries::tests::{ledger_with, req};
    use crate::ledger::{AddRequest, TransferRequest};
    use crate::model::Kind;

    fn f(by: PnlBucket) -> PnlFilter {
        PnlFilter {
            since: None,
            until: None,
            by,
        }
    }

    fn seeded() -> Ledger {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USD", 2)]);
        let day = |d: &str, r: AddRequest| AddRequest {
            ts: Some(format!("2026-09-0{d}")),
            ..r
        };
        l.add(&day("1", req("a", "100", Kind::Deposit))).unwrap();
        l.add(&day(
            "1",
            AddRequest {
                group: Some("g1".into()),
                meta: Some(r#"{"strategy":"arb"}"#.into()),
                ..req("a", "-40", Kind::Trade)
            },
        ))
        .unwrap();
        l.add(&day(
            "2",
            AddRequest {
                group: Some("g1".into()),
                meta: Some(r#"{"strategy":"arb"}"#.into()),
                ..req("a", "48", Kind::Settlement)
            },
        ))
        .unwrap();
        l.add(&day(
            "2",
            AddRequest {
                meta: Some(r#"{"strategy":"mom"}"#.into()),
                ..req("a", "-2", Kind::Fee)
            },
        ))
        .unwrap();
        let bad = l.add(&day("3", req("a", "-7", Kind::Trade))).unwrap().entry;
        l.reverse_entry(bad.id, None, None).unwrap();
        l.transfer(&TransferRequest {
            from: "a".into(),
            to: "b".into(),
            amount: "10".into(),
            reference: None,
            memo: None,
            ts: Some("2026-09-03".into()),
            group: None,
            meta: None,
            actor: None,
        })
        .unwrap();
        l.add(&day("4", req("a", "-30", Kind::Withdrawal))).unwrap();
        l
    }

    #[test]
    fn parses_buckets() {
        assert_eq!(PnlBucket::parse("total").unwrap(), PnlBucket::Total);
        assert_eq!(PnlBucket::parse("day").unwrap(), PnlBucket::Day);
        assert_eq!(
            PnlBucket::parse("meta:strategy").unwrap(),
            PnlBucket::Meta("strategy".into())
        );
        assert!(matches!(
            PnlBucket::parse("meta:"),
            Err(LedgerError::InvalidBucket(_))
        ));
        assert!(matches!(
            PnlBucket::parse("meta:a.b"),
            Err(LedgerError::InvalidBucket(_))
        ));
        assert!(matches!(
            PnlBucket::parse("hour"),
            Err(LedgerError::InvalidBucket(_))
        ));
    }

    #[test]
    fn total_excludes_capital_movements_and_folds_reversals() {
        let l = seeded();
        let out = l.pnl(Some("a"), &f(PnlBucket::Total)).unwrap();
        assert_eq!(out.len(), 1);
        let row = &out[0].rows[0];
        assert_eq!(row.bucket, None);
        assert_eq!(row.trades, "-40.00");
        assert_eq!(row.settlements, "48.00");
        assert_eq!(row.fees, "-2.00");
        assert_eq!(row.adjustments, "0.00");
        assert_eq!(row.other, "0.00");
        assert_eq!(row.net, "6.00");
    }

    #[test]
    fn empty_account_reports_one_zero_row_and_all_accounts_are_listed() {
        let l = seeded();
        let out = l.pnl(None, &f(PnlBucket::Total)).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].account, "b");
        assert_eq!(out[1].rows.len(), 1);
        assert_eq!(out[1].rows[0].net, "0.00");
    }

    #[test]
    fn buckets_by_day_group_and_meta_key() {
        let l = seeded();
        let by_day = l.pnl(Some("a"), &f(PnlBucket::Day)).unwrap();
        let days: Vec<(Option<String>, String)> = by_day[0]
            .rows
            .iter()
            .map(|r| (r.bucket.clone(), r.net.clone()))
            .collect();
        // The -7 trade sits on its own day; its reversal is booked when it happened (now).
        let today = crate::time::now()[..10].to_string();
        assert_eq!(
            days,
            vec![
                (Some("2026-09-01".into()), "-40.00".into()),
                (Some("2026-09-02".into()), "46.00".into()),
                (Some("2026-09-03".into()), "-7.00".into()),
                (Some(today), "7.00".into()),
            ]
        );
        let by_group = l.pnl(Some("a"), &f(PnlBucket::Group)).unwrap();
        let g1 = by_group[0]
            .rows
            .iter()
            .find(|r| r.bucket.as_deref() == Some("g1"))
            .unwrap();
        assert_eq!(g1.net, "8.00");
        let ungrouped = by_group[0]
            .rows
            .iter()
            .find(|r| r.bucket.is_none())
            .unwrap();
        assert_eq!(ungrouped.net, "-2.00");
        let by_meta = l
            .pnl(Some("a"), &f(PnlBucket::Meta("strategy".into())))
            .unwrap();
        let arb = by_meta[0]
            .rows
            .iter()
            .find(|r| r.bucket.as_deref() == Some("arb"))
            .unwrap();
        assert_eq!(arb.net, "8.00");
        let mom = by_meta[0]
            .rows
            .iter()
            .find(|r| r.bucket.as_deref() == Some("mom"))
            .unwrap();
        assert_eq!(mom.fees, "-2.00");
    }

    #[test]
    fn since_until_filter_on_ts() {
        let l = seeded();
        let out = l
            .pnl(
                Some("a"),
                &PnlFilter {
                    since: Some("2026-09-02".into()),
                    until: None,
                    by: PnlBucket::Total,
                },
            )
            .unwrap();
        assert_eq!(out[0].rows[0].net, "46.00");
    }
}
