//! Human-readable tables and CSV export.

use agent_ledger::model::*;

use super::output::Output;

pub fn render(out: &Output) -> String {
    match out {
        Output::Account(r) => {
            let mut s = format!(
                "account {} ({}, {} decimals) id={}",
                r.account.name, r.account.currency, r.account.decimals, r.account.id
            );
            if r.duplicate {
                s.push_str("  (duplicate: account already existed)");
            }
            s.push('\n');
            s
        }
        Output::Accounts { accounts } => table(
            &["id", "name", "currency", "decimals", "note"],
            &accounts
                .iter()
                .map(|a| {
                    vec![
                        a.id.to_string(),
                        a.name.clone(),
                        a.currency.clone(),
                        a.decimals.to_string(),
                        opt(&a.note),
                    ]
                })
                .collect::<Vec<_>>(),
            &[0, 3],
        ),
        Output::Add(r) => {
            let mut s = entries_table(std::slice::from_ref(&r.entry), false);
            s.push_str(&format!(
                "balance {} {}{}\n",
                r.balance,
                r.entry.currency,
                if r.duplicate {
                    "  (duplicate: entry already existed)"
                } else {
                    ""
                }
            ));
            s
        }
        Output::Show(r) => {
            let mut s = entries_table(std::slice::from_ref(&r.entry), false);
            s.push_str(&format!("balance {} {}\n", r.balance, r.entry.currency));
            s
        }
        Output::Transfer(r) => {
            let mut s = entries_table(&r.entries, true);
            if r.duplicate {
                s.push_str("(duplicate: transfer already existed)\n");
            }
            s
        }
        Output::Reverse(r) => entries_table(&r.entries, true),
        Output::Balances { accounts } => table(
            &[
                "account",
                "currency",
                "balance",
                "entries",
                "last_ts",
                "last_reconciled_at",
            ],
            &accounts
                .iter()
                .map(|b| {
                    vec![
                        b.account.clone(),
                        b.currency.clone(),
                        b.balance.clone(),
                        b.entries.to_string(),
                        opt(&b.last_ts),
                        opt(&b.last_reconciled_at),
                    ]
                })
                .collect::<Vec<_>>(),
            &[2, 3],
        ),
        Output::Balance(b) => match &b.at {
            Some(at) => format!("{} {} {} at {}\n", b.account, b.balance, b.currency, at),
            None => format!("{} {} {}\n", b.account, b.balance, b.currency),
        },
        Output::History(h) => history_table(h),
        Output::Group(g) => {
            let mut s = entries_table(&g.entries, true);
            for (ccy, net) in &g.net {
                s.push_str(&format!("net {net} {ccy}\n"));
            }
            s
        }
        Output::Pnl {
            accounts,
            marked,
            total,
        } => {
            let mut headers = vec![
                "bucket",
                "trades",
                "settlements",
                "fees",
                "adjustments",
                "other",
                "net",
            ];
            if *marked {
                headers.extend(["open_value", "mtm"]);
            }
            let right: Vec<usize> = (1..headers.len()).collect();
            let null_label = if *total { "total" } else { "null" };
            let mut s = String::new();
            for a in accounts {
                s.push_str(&format!("{} ({})\n", a.account, a.currency));
                s.push_str(&table(
                    &headers,
                    &a.rows
                        .iter()
                        .map(|r| {
                            let mut cells = vec![
                                r.bucket.clone().unwrap_or_else(|| null_label.into()),
                                r.trades.clone(),
                                r.settlements.clone(),
                                r.fees.clone(),
                                r.adjustments.clone(),
                                r.other.clone(),
                                r.net.clone(),
                            ];
                            if *marked {
                                cells.push(opt(&r.open_value));
                                cells.push(r.mtm.clone());
                            }
                            cells
                        })
                        .collect::<Vec<_>>(),
                    &right,
                ));
            }
            s
        }
        Output::Reconcile(r) => {
            let s = &r.snapshot;
            let head = match s.id {
                Some(id) => format!("snapshot {id}"),
                None => "dry run".to_string(),
            };
            let mut out = format!(
                "{head} {}: observed {} book {} diff {}{}{}\n",
                s.ts,
                s.observed,
                s.book,
                s.diff,
                s.source
                    .as_ref()
                    .map(|src| format!(" ({src})"))
                    .unwrap_or_default(),
                if r.duplicate {
                    "  (duplicate: already recorded)"
                } else {
                    ""
                }
            );
            let diff_is_zero = s.diff.chars().all(|c| matches!(c, '0' | '.' | '-'));
            match &r.adjustment {
                Some(e) => out.push_str(&entries_table(std::slice::from_ref(e), false)),
                None if r.dry_run => out.push_str("nothing written\n"),
                None if diff_is_zero => out.push_str("no adjustment posted\n"),
                None => out.push_str(
                    "no adjustment posted; book the missing activity, or pass --adjust to post the diff\n",
                ),
            }
            out
        }
        Output::Snapshots(l) => table(
            &[
                "id",
                "ts",
                "observed",
                "book",
                "diff",
                "adjustment",
                "source",
            ],
            &l.snapshots
                .iter()
                .map(|s| {
                    vec![
                        s.id.map(|v| v.to_string()).unwrap_or_default(),
                        s.ts.clone(),
                        s.observed.clone(),
                        s.book.clone(),
                        s.diff.clone(),
                        s.adjustment_entry_id
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                        opt(&s.source),
                    ]
                })
                .collect::<Vec<_>>(),
            &[0, 2, 3, 4, 5],
        ),
        Output::Import(r) => {
            let mut s = format!(
                "imported {} duplicates {}{}\n",
                r.imported,
                r.duplicates,
                if r.dry_run {
                    "  (dry run, nothing written)"
                } else {
                    ""
                }
            );
            s.push_str(&entries_table(&r.entries, true));
            s
        }
        Output::Raw(text) => text.clone(),
    }
}

pub fn csv(history: &History) -> String {
    let mut out = String::from(
        "id,ts,recorded_at,kind,amount,balance_after,ref,memo,actor,group,meta,reverses_id,reversed_by\n",
    );
    for h in &history.entries {
        let e = &h.entry;
        let fields = [
            e.id.to_string(),
            e.ts.clone(),
            e.recorded_at.clone(),
            e.kind.as_str().to_string(),
            e.amount.clone(),
            h.balance_after.clone(),
            opt(&e.reference),
            opt(&e.memo),
            opt(&e.actor),
            opt(&e.group_id),
            e.meta.as_ref().map(|m| m.to_string()).unwrap_or_default(),
            e.reverses_id.map(|v| v.to_string()).unwrap_or_default(),
            e.reversed_by.map(|v| v.to_string()).unwrap_or_default(),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn opt(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

fn rev(e: &Entry) -> String {
    match (e.reverses_id, e.reversed_by) {
        (Some(id), _) => format!("reverses {id}"),
        (None, Some(by)) => format!("reversed by {by}"),
        (None, None) => String::new(),
    }
}

fn entries_table(entries: &[Entry], with_account: bool) -> String {
    let mut headers = vec!["id"];
    if with_account {
        headers.push("account");
    }
    headers.extend([
        "ts", "kind", "amount", "ref", "group", "memo", "actor", "reversal",
    ]);
    let rows = entries
        .iter()
        .map(|e| {
            let mut row = vec![e.id.to_string()];
            if with_account {
                row.push(e.account.clone());
            }
            row.extend([
                e.ts.clone(),
                e.kind.as_str().to_string(),
                e.amount.clone(),
                opt(&e.reference),
                opt(&e.group_id),
                opt(&e.memo),
                opt(&e.actor),
                rev(e),
            ]);
            row
        })
        .collect::<Vec<_>>();
    let amount_col = if with_account { 4 } else { 3 };
    table(&headers, &rows, &[0, amount_col])
}

fn history_table(h: &History) -> String {
    let mut s = format!("{} ({})\n", h.account, h.currency);
    let rows = h
        .entries
        .iter()
        .map(|x| {
            let e = &x.entry;
            vec![
                e.id.to_string(),
                e.ts.clone(),
                e.kind.as_str().to_string(),
                e.amount.clone(),
                x.balance_after.clone(),
                opt(&e.reference),
                opt(&e.group_id),
                opt(&e.memo),
                opt(&e.actor),
                rev(e),
            ]
        })
        .collect::<Vec<_>>();
    s.push_str(&table(
        &[
            "id", "ts", "kind", "amount", "balance", "ref", "group", "memo", "actor", "reversal",
        ],
        &rows,
        &[0, 3, 4],
    ));
    s
}

/// Aligned text table. `right` lists column indexes that are right-aligned.
fn table(headers: &[&str], rows: &[Vec<String>], right: &[usize]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let fmt_row = |cells: &[String]| -> String {
        let mut parts = Vec::with_capacity(cols);
        for (i, width) in widths.iter().enumerate() {
            let cell = cells.get(i).map(String::as_str).unwrap_or("");
            let pad = width - cell.chars().count();
            if right.contains(&i) {
                parts.push(format!("{}{}", " ".repeat(pad), cell));
            } else {
                parts.push(format!("{}{}", cell, " ".repeat(pad)));
            }
        }
        parts.join("  ").trim_end().to_string()
    };
    let mut out = String::new();
    out.push_str(&fmt_row(
        &headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
    ));
    out.push('\n');
    out.push_str(&fmt_row(
        &widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>(),
    ));
    out.push('\n');
    for row in rows {
        out.push_str(&fmt_row(row));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_aligns_columns_and_right_aligns_numbers() {
        let t = table(
            &["id", "amount"],
            &[
                vec!["1".into(), "-25.50".into()],
                vec!["12".into(), "100.00".into()],
            ],
            &[1],
        );
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "id  amount");
        assert_eq!(lines[1], "--  ------");
        assert_eq!(lines[2], "1   -25.50");
        assert_eq!(lines[3], "12  100.00");
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a, b"), "\"a, b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
    }
}
