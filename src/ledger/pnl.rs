//! Realized cash PnL, excluding capital movements, optionally joined with caller-supplied marks.

use std::collections::{BTreeMap, HashMap};

use rusqlite::types::Value;

use super::{
    account_by_name, all_accounts, normalize_opt_ts, parse_account_amount, validate_group,
    AccountRow, Ledger,
};
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

/// Caller-supplied values of open positions, keyed by group id. Amounts stay text until the
/// account that owns the group, and so its decimals, is known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks(BTreeMap<String, String>);

impl Marks {
    /// Parses `{"<group>": "<amount>", ...}`. Amounts must be JSON strings, as everywhere else.
    pub fn parse(text: &str) -> Result<Marks> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| LedgerError::InvalidJson(e.to_string()))?;
        let Some(object) = value.as_object() else {
            return Err(LedgerError::InvalidJson(
                "marks must be a JSON object of group id to amount".into(),
            ));
        };
        let mut marks = BTreeMap::new();
        for (group, amount) in object {
            let group = validate_group(Some(group))?.unwrap_or_default();
            let amount = match amount {
                serde_json::Value::String(s) => s.clone(),
                other => {
                    return Err(LedgerError::InvalidAmount(format!(
                        "{other} (amount must be a JSON string)"
                    )))
                }
            };
            marks.insert(group, amount);
        }
        Ok(Marks(marks))
    }
}

#[derive(Clone, Debug)]
pub struct PnlFilter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub by: PnlBucket,
    pub marks: Option<Marks>,
}

impl Ledger {
    pub fn pnl(&self, account: Option<&str>, filter: &PnlFilter) -> Result<Vec<AccountPnl>> {
        let accounts: Vec<AccountRow> = match account {
            Some(name) => vec![account_by_name(&self.conn, name)?],
            None => all_accounts(&self.conn)?,
        };
        let since = normalize_opt_ts(filter.since.as_deref())?;
        let until = normalize_opt_ts(filter.until.as_deref())?;
        if let Some(marks) = &filter.marks {
            self.check_marked_groups_exist(marks)?;
        }
        let sql = format!(
            "SELECT {bucket} AS bucket, \
                    COALESCE(SUM(CASE WHEN x.k = 'trade' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'settlement' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'fee' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'adjustment' THEN x.amount END), 0), \
                    COALESCE(SUM(CASE WHEN x.k = 'other' THEN x.amount END), 0), \
                    COALESCE(SUM(x.amount), 0), \
                    json_group_array(DISTINCT x.group_id) \
             FROM (SELECT e.ts, e.amount, e.group_id, e.meta, COALESCE(o.kind, e.kind) AS k \
                   FROM entries e LEFT JOIN entries o ON o.id = e.reverses_id \
                   WHERE e.account_id = ?1) x \
             WHERE x.k NOT IN ('deposit', 'withdrawal', 'transfer') \
               AND (?2 IS NULL OR x.ts >= ?2) AND (?3 IS NULL OR x.ts <= ?3) \
             GROUP BY bucket ORDER BY bucket",
            bucket = filter.by.sql_expr()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        // Marked group -> the row that already took its mark, to catch a group spanning rows.
        let mut taken: HashMap<String, String> = HashMap::new();
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
            let raw = stmt
                .query_map(rusqlite::params_from_iter(values), |r| {
                    let bucket: Option<String> = r.get(0)?;
                    let sums = [
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ];
                    let groups: String = r.get(7)?;
                    Ok((bucket, sums, groups))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut rows = Vec::with_capacity(raw.len());
            for (bucket, sums, groups) in raw {
                let open_value = match &filter.marks {
                    Some(marks) => open_value(marks, &groups, &acc, bucket.as_deref(), &mut taken)?,
                    None => None,
                };
                rows.push(pnl_row(bucket, sums, open_value, acc.decimals));
            }
            if rows.is_empty() && filter.by == PnlBucket::Total {
                rows.push(pnl_row(None, [0; 6], None, acc.decimals));
            }
            out.push(AccountPnl {
                account: acc.name,
                currency: acc.currency,
                rows,
            });
        }
        Ok(out)
    }

    fn check_marked_groups_exist(&self, marks: &Marks) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM entries WHERE group_id = ?1 LIMIT 1")?;
        for group in marks.0.keys() {
            if !stmt.exists([group])? {
                return Err(LedgerError::GroupNotFound(group.clone()));
            }
        }
        Ok(())
    }
}

/// Sums the marks of the groups present in one pnl row. `groups_json` is the JSON array
/// `json_group_array` produced; a `null` element is the ungrouped entries.
fn open_value(
    marks: &Marks,
    groups_json: &str,
    acc: &AccountRow,
    bucket: Option<&str>,
    taken: &mut HashMap<String, String>,
) -> Result<Option<i64>> {
    let groups: Vec<Option<String>> =
        serde_json::from_str(groups_json).expect("json_group_array yields a JSON array");
    let row = format!("{}/{}", acc.name, bucket.unwrap_or("total"));
    let mut total: Option<i64> = None;
    for group in groups.into_iter().flatten() {
        let Some(amount) = marks.0.get(&group) else {
            continue;
        };
        if let Some(earlier) = taken.insert(group.clone(), row.clone()) {
            return Err(LedgerError::MarkAmbiguous {
                group,
                rows: format!("{earlier}, {row}"),
            });
        }
        let minor = parse_account_amount(amount, acc)?;
        total = Some(total.unwrap_or(0) + minor);
    }
    Ok(total)
}

fn pnl_row(bucket: Option<String>, sums: [i64; 6], open_value: Option<i64>, d: u32) -> PnlRow {
    let [trades, settlements, fees, adjustments, other, net] = sums;
    PnlRow {
        bucket,
        trades: format_amount(trades, d),
        settlements: format_amount(settlements, d),
        fees: format_amount(fees, d),
        adjustments: format_amount(adjustments, d),
        other: format_amount(other, d),
        net: format_amount(net, d),
        open_value: open_value.map(|v| format_amount(v, d)),
        mtm: format_amount(net + open_value.unwrap_or(0), d),
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
            marks: None,
        }
    }

    fn fm(by: PnlBucket, marks: &str) -> PnlFilter {
        PnlFilter {
            marks: Some(Marks::parse(marks).unwrap()),
            ..f(by)
        }
    }

    fn row<'a>(out: &'a [AccountPnl], account: &str, bucket: Option<&str>) -> &'a PnlRow {
        out.iter()
            .find(|a| a.account == account)
            .unwrap()
            .rows
            .iter()
            .find(|r| r.bucket.as_deref() == bucket)
            .unwrap()
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
                    marks: None,
                },
            )
            .unwrap();
        assert_eq!(out[0].rows[0].net, "46.00");
    }

    #[test]
    fn without_marks_rows_carry_null_open_value_and_mtm_equals_net() {
        let l = seeded();
        let out = l.pnl(Some("a"), &f(PnlBucket::Group)).unwrap();
        for r in &out[0].rows {
            assert_eq!(r.open_value, None);
            assert_eq!(r.mtm, r.net);
        }
    }

    #[test]
    fn marks_join_open_value_into_group_rows() {
        let l = seeded();
        let out = l
            .pnl(Some("a"), &fm(PnlBucket::Group, r#"{"g1":"5.00"}"#))
            .unwrap();
        let g1 = row(&out, "a", Some("g1"));
        assert_eq!(g1.net, "8.00");
        assert_eq!(g1.open_value.as_deref(), Some("5.00"));
        assert_eq!(g1.mtm, "13.00");
        let ungrouped = row(&out, "a", None);
        assert_eq!(ungrouped.open_value, None);
        assert_eq!(ungrouped.mtm, "-2.00");
    }

    #[test]
    fn marks_sum_into_total_and_meta_buckets() {
        let l = seeded();
        let total = l
            .pnl(Some("a"), &fm(PnlBucket::Total, r#"{"g1":"5.00"}"#))
            .unwrap();
        let t = row(&total, "a", None);
        assert_eq!(t.net, "6.00");
        assert_eq!(t.open_value.as_deref(), Some("5.00"));
        assert_eq!(t.mtm, "11.00");

        let by_meta = l
            .pnl(
                Some("a"),
                &fm(PnlBucket::Meta("strategy".into()), r#"{"g1":"5.00"}"#),
            )
            .unwrap();
        let arb = row(&by_meta, "a", Some("arb"));
        assert_eq!(arb.open_value.as_deref(), Some("5.00"));
        assert_eq!(arb.mtm, "13.00");
        let mom = row(&by_meta, "a", Some("mom"));
        assert_eq!(mom.open_value, None);
        assert_eq!(mom.mtm, "-2.00");
    }

    #[test]
    fn zero_and_negative_marks_are_allowed() {
        let l = seeded();
        let out = l
            .pnl(Some("a"), &fm(PnlBucket::Group, r#"{"g1":"0"}"#))
            .unwrap();
        let g1 = row(&out, "a", Some("g1"));
        assert_eq!(g1.open_value.as_deref(), Some("0.00"));
        assert_eq!(g1.mtm, "8.00");
        let out = l
            .pnl(Some("a"), &fm(PnlBucket::Group, r#"{"g1":"-1.5"}"#))
            .unwrap();
        assert_eq!(row(&out, "a", Some("g1")).mtm, "6.50");
    }

    #[test]
    fn mark_for_group_that_exists_nowhere_is_group_not_found() {
        let l = seeded();
        let err = l
            .pnl(Some("a"), &fm(PnlBucket::Group, r#"{"nope":"1"}"#))
            .unwrap_err();
        assert!(matches!(err, LedgerError::GroupNotFound(g) if g == "nope"));
    }

    #[test]
    fn mark_for_group_outside_the_report_is_ignored() {
        let l = seeded();
        // g1 lives on account a; reporting b alone leaves the mark unused.
        let out = l
            .pnl(Some("b"), &fm(PnlBucket::Total, r#"{"g1":"5.00"}"#))
            .unwrap();
        let b = row(&out, "b", None);
        assert_eq!(b.open_value, None);
        assert_eq!(b.mtm, "0.00");
        // g1's entries are all before 2026-09-03, so a window after that ignores it too.
        let out = l
            .pnl(
                Some("a"),
                &PnlFilter {
                    since: Some("2026-09-03".into()),
                    ..fm(PnlBucket::Total, r#"{"g1":"5.00"}"#)
                },
            )
            .unwrap();
        assert_eq!(row(&out, "a", None).open_value, None);
    }

    #[test]
    fn mark_for_group_spanning_rows_is_ambiguous() {
        let mut l = seeded();
        // g1 has entries on two days.
        let err = l
            .pnl(Some("a"), &fm(PnlBucket::Day, r#"{"g1":"5.00"}"#))
            .unwrap_err();
        assert!(matches!(&err, LedgerError::MarkAmbiguous { group, .. } if group == "g1"));
        assert_eq!(err.code(), "mark_ambiguous");
        // A group traded on two accounts cannot take one mark either.
        l.add(&AddRequest {
            group: Some("g1".into()),
            ..req("b", "-3", Kind::Trade)
        })
        .unwrap();
        let err = l
            .pnl(None, &fm(PnlBucket::Group, r#"{"g1":"5.00"}"#))
            .unwrap_err();
        assert!(matches!(&err, LedgerError::MarkAmbiguous { group, .. } if group == "g1"));
    }

    #[test]
    fn mark_precision_follows_the_account_decimals() {
        let l = seeded();
        let err = l
            .pnl(Some("a"), &fm(PnlBucket::Group, r#"{"g1":"5.001"}"#))
            .unwrap_err();
        assert!(matches!(
            err,
            LedgerError::PrecisionExceeded {
                scale: 3,
                decimals: 2,
                ..
            }
        ));
    }

    #[test]
    fn marks_must_be_an_object_of_amount_strings() {
        assert!(matches!(
            Marks::parse("[1]"),
            Err(LedgerError::InvalidJson(_))
        ));
        assert!(matches!(
            Marks::parse("not json"),
            Err(LedgerError::InvalidJson(_))
        ));
        assert!(matches!(
            Marks::parse(r#"{"g1": 5}"#),
            Err(LedgerError::InvalidAmount(_))
        ));
        assert!(matches!(
            Marks::parse(r#"{"": "5"}"#),
            Err(LedgerError::InvalidGroup)
        ));
        assert!(Marks::parse("{}").is_ok());
    }
}
