//! Bulk import from JSON Lines, all-or-nothing.

use std::io::BufRead;

use serde::Deserialize;

use super::{
    account_by_name, clean_ref, load_entry, meta_value_to_storage, parse_account_amount,
    resolve_ts, validate_group, write_entry, Ledger, NewEntry, Written,
};
use crate::error::{LedgerError, Result};
use crate::model::{ImportResult, Kind};

#[derive(Debug, Deserialize)]
struct ImportLine {
    account: String,
    amount: serde_json::Value,
    kind: String,
    #[serde(rename = "ref")]
    reference: Option<String>,
    ts: Option<String>,
    group: Option<String>,
    memo: Option<String>,
    meta: Option<serde_json::Value>,
    actor: Option<String>,
}

fn at_line(line: usize, e: LedgerError) -> LedgerError {
    LedgerError::Import {
        line,
        source: Box::new(e),
    }
}

impl Ledger {
    pub fn import<R: BufRead>(
        &mut self,
        reader: R,
        dry_run: bool,
        default_actor: Option<&str>,
    ) -> Result<ImportResult> {
        let mut lines: Vec<(usize, ImportLine)> = Vec::new();
        for (idx, raw) in reader.lines().enumerate() {
            let n = idx + 1;
            let raw = raw?;
            if raw.trim().is_empty() {
                continue;
            }
            let parsed: ImportLine = serde_json::from_str(&raw)
                .map_err(|e| at_line(n, LedgerError::InvalidJson(e.to_string())))?;
            lines.push((n, parsed));
        }

        let tx = self.write_tx()?;
        let mut imported = 0;
        let mut duplicates = 0;
        let mut entries = Vec::with_capacity(lines.len());
        for (n, line) in &lines {
            let written = (|| -> Result<Written> {
                let account = account_by_name(&tx, &line.account)?;
                let amount_text = match &line.amount {
                    serde_json::Value::String(s) => s.as_str(),
                    other => {
                        return Err(LedgerError::InvalidAmount(format!(
                            "{other} (amount must be a JSON string)"
                        )))
                    }
                };
                let meta = match &line.meta {
                    None => None,
                    Some(v) => Some(meta_value_to_storage(v)?),
                };
                let new = NewEntry {
                    account: &account,
                    ts: resolve_ts(line.ts.as_deref())?,
                    kind: Kind::parse_addable(&line.kind)?,
                    amount: parse_account_amount(amount_text, &account)?,
                    reference: clean_ref(line.reference.as_deref()),
                    memo: line.memo.clone(),
                    actor: line
                        .actor
                        .clone()
                        .or_else(|| default_actor.map(str::to_string)),
                    group_id: validate_group(line.group.as_deref())?,
                    meta,
                    reverses_id: None,
                };
                write_entry(&tx, &new)
            })()
            .map_err(|e| at_line(*n, e))?;
            let id = match written {
                Written::Inserted(id) => {
                    imported += 1;
                    id
                }
                Written::Duplicate(id) => {
                    duplicates += 1;
                    id
                }
            };
            entries.push(load_entry(&tx, id)?);
        }
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()?;
        }
        Ok(ImportResult {
            imported,
            duplicates,
            dry_run,
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ledger::entries::tests::ledger_with;
    use crate::ledger::HistoryFilter;

    const LINES: &str = r#"
{"account":"poly","amount":"100","kind":"deposit","ref":"0xdep","ts":"2026-09-01","actor":"backfill"}
{"account":"poly","amount":"-25.500000","kind":"trade","ref":"o1","group":"arb:1","meta":{"market":"btc-5m","side":"buy"}}

{"account":"kalshi","amount":"-24","kind":"trade","ref":"k1","group":"arb:1"}
"#;

    #[test]
    fn imports_all_lines_in_one_batch_and_skips_duplicates_on_replay() {
        let mut l = ledger_with(&[("poly", "USDC", 6), ("kalshi", "USD", 2)]);
        let r = l.import(LINES.as_bytes(), false, Some("claude")).unwrap();
        assert_eq!(r.imported, 3);
        assert_eq!(r.duplicates, 0);
        assert!(!r.dry_run);
        assert_eq!(r.entries[0].actor.as_deref(), Some("backfill"));
        assert_eq!(r.entries[1].actor.as_deref(), Some("claude"));
        assert_eq!(r.entries[1].meta.as_ref().unwrap()["side"], "buy");
        assert_eq!(l.balance("poly", None).unwrap().balance, "74.500000");

        let again = l.import(LINES.as_bytes(), false, None).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.duplicates, 3);
        assert_eq!(
            l.history("poly", &HistoryFilter::default())
                .unwrap()
                .entries
                .len(),
            2
        );
    }

    #[test]
    fn dry_run_reports_but_writes_nothing() {
        let mut l = ledger_with(&[("poly", "USDC", 6), ("kalshi", "USD", 2)]);
        let r = l.import(LINES.as_bytes(), true, None).unwrap();
        assert_eq!(r.imported, 3);
        assert!(r.dry_run);
        assert_eq!(l.balance("poly", None).unwrap().balance, "0.000000");
    }

    #[test]
    fn any_error_rolls_back_everything_with_line_number() {
        let mut l = ledger_with(&[("poly", "USDC", 6)]);
        let bad = "{\"account\":\"poly\",\"amount\":\"1\",\"kind\":\"deposit\"}\n{\"account\":\"poly\",\"amount\":\"-1\",\"kind\":\"deposit\"}\n";
        let err = l.import(bad.as_bytes(), false, None).unwrap_err();
        assert_eq!(err.line(), Some(2));
        assert_eq!(err.code(), "invalid_sign");
        assert_eq!(l.balance("poly", None).unwrap().balance, "0.000000");

        let not_json = "{\"account\":\"poly\",\"amount\":\"1\",\"kind\":\"deposit\"}\nnope\n";
        let err = l.import(not_json.as_bytes(), false, None).unwrap_err();
        assert_eq!(err.line(), Some(2));
        assert_eq!(err.code(), "invalid_json");

        let numeric = "{\"account\":\"poly\",\"amount\":1.5,\"kind\":\"deposit\"}\n";
        let err = l.import(numeric.as_bytes(), false, None).unwrap_err();
        assert_eq!(err.code(), "invalid_amount");

        let transfer = "{\"account\":\"poly\",\"amount\":\"1\",\"kind\":\"transfer\"}\n";
        assert_eq!(
            l.import(transfer.as_bytes(), false, None)
                .unwrap_err()
                .code(),
            "invalid_kind"
        );

        let bad_meta =
            "{\"account\":\"poly\",\"amount\":\"1\",\"kind\":\"deposit\",\"meta\":[1]}\n";
        assert_eq!(
            l.import(bad_meta.as_bytes(), false, None)
                .unwrap_err()
                .code(),
            "invalid_meta"
        );
        assert_eq!(l.balance("poly", None).unwrap().balance, "0.000000");
    }

    #[test]
    fn same_ref_twice_in_one_batch_is_a_duplicate() {
        let mut l = ledger_with(&[("poly", "USDC", 6)]);
        let twice = "{\"account\":\"poly\",\"amount\":\"1\",\"kind\":\"deposit\",\"ref\":\"x\"}\n{\"account\":\"poly\",\"amount\":\"1\",\"kind\":\"deposit\",\"ref\":\"x\"}\n";
        let r = l.import(twice.as_bytes(), false, None).unwrap();
        assert_eq!((r.imported, r.duplicates), (1, 1));
    }
}
