//! Writing entries: add, show, transfer, reverse.

use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::{
    account_by_name, balance_minor, clean_ref, existing_ref, load_entry, parse_account_amount,
    resolve_ts, validate_group, validate_meta, write_entry, Ledger, NewEntry, Written,
};
use crate::error::{LedgerError, Result};
use crate::model::{AddResult, Entry, Kind, ReverseResult, ShowResult, TransferResult};
use crate::money::format_amount;
use crate::time;

#[derive(Clone, Debug)]
pub struct AddRequest {
    pub account: String,
    pub amount: String,
    pub kind: Kind,
    pub reference: Option<String>,
    pub memo: Option<String>,
    pub ts: Option<String>,
    pub group: Option<String>,
    pub meta: Option<String>,
    pub actor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TransferRequest {
    pub from: String,
    pub to: String,
    pub amount: String,
    pub reference: Option<String>,
    pub memo: Option<String>,
    pub ts: Option<String>,
    pub group: Option<String>,
    pub meta: Option<String>,
    pub actor: Option<String>,
}

impl Ledger {
    pub fn add(&mut self, req: &AddRequest) -> Result<AddResult> {
        let tx = self.write_tx()?;
        let account = account_by_name(&tx, &req.account)?;
        let new = NewEntry {
            account: &account,
            ts: resolve_ts(req.ts.as_deref())?,
            kind: req.kind,
            amount: parse_account_amount(&req.amount, &account)?,
            reference: clean_ref(req.reference.as_deref()),
            memo: req.memo.clone(),
            actor: req.actor.clone(),
            group_id: validate_group(req.group.as_deref())?,
            meta: validate_meta(req.meta.as_deref())?,
            reverses_id: None,
        };
        let (id, duplicate) = match write_entry(&tx, &new)? {
            Written::Inserted(id) => (id, false),
            Written::Duplicate(id) => (id, true),
        };
        let entry = load_entry(&tx, id)?;
        let balance = format_amount(balance_minor(&tx, account.id, None)?, account.decimals);
        tx.commit()?;
        Ok(AddResult {
            entry,
            balance,
            duplicate,
        })
    }

    pub fn show(&self, id: i64) -> Result<ShowResult> {
        let entry = load_entry(&self.conn, id)?;
        let account = account_by_name(&self.conn, &entry.account)?;
        let balance = format_amount(
            balance_minor(&self.conn, account.id, None)?,
            account.decimals,
        );
        Ok(ShowResult { entry, balance })
    }
}

impl Ledger {
    pub fn transfer(&mut self, req: &TransferRequest) -> Result<TransferResult> {
        let tx = self.write_tx()?;
        let from = account_by_name(&tx, &req.from)?;
        let to = account_by_name(&tx, &req.to)?;
        if from.id == to.id {
            return Err(LedgerError::SameAccount);
        }
        if from.currency != to.currency {
            return Err(LedgerError::CurrencyMismatch {
                from: from.name,
                from_currency: from.currency,
                to: to.name,
                to_currency: to.currency,
            });
        }
        let amount = parse_account_amount(&req.amount, &from)?;
        if amount == 0 {
            return Err(LedgerError::ZeroAmount);
        }
        if amount < 0 {
            return Err(LedgerError::InvalidSign {
                kind: "transfer".into(),
                expected: "positive",
                amount: format_amount(amount, from.decimals),
            });
        }
        let ts = resolve_ts(req.ts.as_deref())?;
        let group =
            validate_group(req.group.as_deref())?.unwrap_or_else(|| Uuid::new_v4().to_string());
        let meta = validate_meta(req.meta.as_deref())?;
        let reference = clean_ref(req.reference.as_deref());

        if let Some(r) = &reference {
            let a = existing_ref(&tx, from.id, r)?;
            let b = existing_ref(&tx, to.id, r)?;
            match (a, b) {
                (Some(x), Some(y))
                    if x.kind == "transfer"
                        && y.kind == "transfer"
                        && x.amount == -amount
                        && y.amount == amount =>
                {
                    let entries = vec![load_entry(&tx, x.id)?, load_entry(&tx, y.id)?];
                    return Ok(TransferResult {
                        entries,
                        duplicate: true,
                    });
                }
                (None, None) => {}
                (Some(x), _) => return Err(conflict(&from, r, x)),
                (_, Some(y)) => return Err(conflict(&to, r, y)),
            }
        }

        let mut entries = Vec::with_capacity(2);
        for (account, signed) in [(&from, -amount), (&to, amount)] {
            let new = NewEntry {
                account,
                ts: ts.clone(),
                kind: Kind::Transfer,
                amount: signed,
                reference: reference.clone(),
                memo: req.memo.clone(),
                actor: req.actor.clone(),
                group_id: Some(group.clone()),
                meta: meta.clone(),
                reverses_id: None,
            };
            let Written::Inserted(id) = write_entry(&tx, &new)? else {
                unreachable!("refs were checked above")
            };
            entries.push(load_entry(&tx, id)?);
        }
        tx.commit()?;
        Ok(TransferResult {
            entries,
            duplicate: false,
        })
    }

    pub fn reverse_entry(
        &mut self,
        id: i64,
        memo: Option<String>,
        actor: Option<String>,
    ) -> Result<ReverseResult> {
        let tx = self.write_tx()?;
        let original = load_entry(&tx, id)?;
        let mut targets = vec![original.clone()];
        if original.kind == Kind::Transfer {
            if let Some(group) = &original.group_id {
                let sibling: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM entries WHERE group_id = ?1 AND kind = 'transfer' AND id <> ?2",
                        params![group, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(sid) = sibling {
                    targets.push(load_entry(&tx, sid)?);
                }
            }
        }
        let entries = reverse_all(&tx, &targets, memo, actor)?;
        tx.commit()?;
        Ok(ReverseResult { entries })
    }

    pub fn reverse_group(
        &mut self,
        group: &str,
        memo: Option<String>,
        actor: Option<String>,
    ) -> Result<ReverseResult> {
        let group = validate_group(Some(group))?.expect("Some in, Some out");
        let tx = self.write_tx()?;
        let total: i64 = tx.query_row(
            "SELECT count(*) FROM entries WHERE group_id = ?1",
            params![&group],
            |r| r.get(0),
        )?;
        if total == 0 {
            return Err(LedgerError::GroupNotFound(group));
        }
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare(
                "SELECT e.id FROM entries e WHERE e.group_id = ?1 AND e.kind <> 'reversal' \
                 AND NOT EXISTS (SELECT 1 FROM entries r WHERE r.reverses_id = e.id) ORDER BY e.id",
            )?;
            let rows = stmt.query_map(params![&group], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if ids.is_empty() {
            return Err(LedgerError::NothingToReverse(group));
        }
        let targets = ids
            .into_iter()
            .map(|id| load_entry(&tx, id))
            .collect::<Result<Vec<_>>>()?;
        let entries = reverse_all(&tx, &targets, memo, actor)?;
        tx.commit()?;
        Ok(ReverseResult { entries })
    }
}

fn conflict(
    account: &super::AccountRow,
    reference: &str,
    existing: super::ExistingRef,
) -> LedgerError {
    LedgerError::RefConflict {
        account: account.name.clone(),
        reference: reference.to_string(),
        existing_id: existing.id,
        existing_kind: existing.kind,
        existing_amount: format_amount(existing.amount, account.decimals),
    }
}

fn reverse_all(
    tx: &rusqlite::Connection,
    targets: &[Entry],
    memo: Option<String>,
    actor: Option<String>,
) -> Result<Vec<Entry>> {
    for t in targets {
        if t.kind == Kind::Reversal {
            return Err(LedgerError::CannotReverseReversal(t.id));
        }
        if let Some(by) = t.reversed_by {
            return Err(LedgerError::AlreadyReversed(t.id, by));
        }
    }
    let ts = time::now();
    let mut out = Vec::with_capacity(targets.len());
    for t in targets {
        let account = account_by_name(tx, &t.account)?;
        let new = NewEntry {
            account: &account,
            ts: ts.clone(),
            kind: Kind::Reversal,
            amount: -t.amount_minor,
            reference: None,
            memo: memo.clone(),
            actor: actor.clone(),
            group_id: t.group_id.clone(),
            meta: None,
            reverses_id: Some(t.id),
        };
        let Written::Inserted(id) = write_entry(tx, &new)? else {
            unreachable!("reversals carry no ref")
        };
        out.push(load_entry(tx, id)?);
    }
    Ok(out)
}

// Temporary until reports.rs (Task 7) provides the real `balance`.
#[cfg(test)]
impl Ledger {
    pub(crate) fn balance(
        &self,
        account: &str,
        at: Option<&str>,
    ) -> Result<crate::model::BalanceAt> {
        let acc = account_by_name(&self.conn, account)?;
        let minor = balance_minor(&self.conn, acc.id, at)?;
        Ok(crate::model::BalanceAt {
            account: acc.name,
            currency: acc.currency,
            balance: format_amount(minor, acc.decimals),
            at: at.map(str::to_string),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn ledger_with(accounts: &[(&str, &str, u32)]) -> Ledger {
        let mut l = Ledger::open_in_memory().unwrap();
        for (name, ccy, dec) in accounts {
            l.add_account(name, ccy, *dec, None).unwrap();
        }
        l
    }

    pub(crate) fn req(account: &str, amount: &str, kind: Kind) -> AddRequest {
        AddRequest {
            account: account.into(),
            amount: amount.into(),
            kind,
            reference: None,
            memo: None,
            ts: None,
            group: None,
            meta: None,
            actor: None,
        }
    }

    #[test]
    fn add_returns_entry_and_running_balance() {
        let mut l = ledger_with(&[("poly-usdc", "USDC", 6)]);
        let r = l
            .add(&AddRequest {
                reference: Some("0xabc".into()),
                actor: Some("claude".into()),
                ..req("poly-usdc", "100", Kind::Deposit)
            })
            .unwrap();
        assert_eq!(r.entry.amount, "100.000000");
        assert_eq!(r.entry.currency, "USDC");
        assert_eq!(r.entry.actor.as_deref(), Some("claude"));
        assert_eq!(r.balance, "100.000000");
        assert!(!r.duplicate);
        let r2 = l.add(&req("POLY-USDC", "-25.5", Kind::Trade)).unwrap();
        assert_eq!(r2.balance, "74.500000");
        assert_eq!(r2.entry.id, 2);
    }

    #[test]
    fn same_ref_same_amount_is_duplicate_different_amount_conflicts() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        let first = l
            .add(&AddRequest {
                reference: Some("tx1".into()),
                ..req("w", "10", Kind::Deposit)
            })
            .unwrap();
        let again = l
            .add(&AddRequest {
                reference: Some("tx1".into()),
                ..req("w", "10.00", Kind::Deposit)
            })
            .unwrap();
        assert!(again.duplicate);
        assert_eq!(again.entry.id, first.entry.id);
        assert_eq!(again.balance, "10.00");
        let conflict = l.add(&AddRequest {
            reference: Some("tx1".into()),
            ..req("w", "11", Kind::Deposit)
        });
        assert!(matches!(
            conflict,
            Err(LedgerError::RefConflict { existing_id: 1, .. })
        ));
        assert_eq!(l.balance("w", None).unwrap().balance, "10.00");
    }

    #[test]
    fn validates_sign_precision_kind_group_meta_and_ts() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        assert!(matches!(
            l.add(&req("w", "-1", Kind::Deposit)),
            Err(LedgerError::InvalidSign { .. })
        ));
        assert!(matches!(
            l.add(&req("w", "1", Kind::Fee)),
            Err(LedgerError::InvalidSign { .. })
        ));
        assert!(matches!(
            l.add(&req("w", "0", Kind::Trade)),
            Err(LedgerError::ZeroAmount)
        ));
        assert!(matches!(
            l.add(&req("w", "1.001", Kind::Trade)),
            Err(LedgerError::PrecisionExceeded { scale: 3, .. })
        ));
        assert!(matches!(
            l.add(&req("w", "abc", Kind::Trade)),
            Err(LedgerError::InvalidAmount(_))
        ));
        assert!(matches!(
            l.add(&AddRequest {
                group: Some("  ".into()),
                ..req("w", "1", Kind::Trade)
            }),
            Err(LedgerError::InvalidGroup)
        ));
        assert!(matches!(
            l.add(&AddRequest {
                meta: Some("[1]".into()),
                ..req("w", "1", Kind::Trade)
            }),
            Err(LedgerError::InvalidMeta(_))
        ));
        assert!(matches!(
            l.add(&AddRequest {
                ts: Some("later".into()),
                ..req("w", "1", Kind::Trade)
            }),
            Err(LedgerError::InvalidTimestamp(_))
        ));
        assert!(matches!(
            l.add(&req("nope", "1", Kind::Trade)),
            Err(LedgerError::AccountNotFound(_))
        ));
        assert_eq!(l.balance("w", None).unwrap().balance, "0.00");
    }

    #[test]
    fn stores_ts_group_and_meta_verbatim() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        let r = l
            .add(&AddRequest {
                ts: Some("2026-09-06T12:00:00+09:00".into()),
                group: Some("arb:1".into()),
                meta: Some(r#"{"market":"btc-5m","price":"0.51"}"#.into()),
                memo: Some("buy".into()),
                ..req("w", "-5", Kind::Trade)
            })
            .unwrap();
        assert_eq!(r.entry.ts, "2026-09-06T03:00:00.000Z");
        assert_eq!(r.entry.group_id.as_deref(), Some("arb:1"));
        assert_eq!(r.entry.meta.as_ref().unwrap()["market"], "btc-5m");
        let shown = l.show(r.entry.id).unwrap();
        assert_eq!(shown.entry.memo.as_deref(), Some("buy"));
        assert_eq!(shown.balance, "-5.00");
        assert!(matches!(l.show(99), Err(LedgerError::EntryNotFound(99))));
    }

    fn treq(from: &str, to: &str, amount: &str) -> TransferRequest {
        TransferRequest {
            from: from.into(),
            to: to.into(),
            amount: amount.into(),
            reference: None,
            memo: None,
            ts: None,
            group: None,
            meta: None,
            actor: None,
        }
    }

    #[test]
    fn transfer_writes_two_linked_legs_atomically() {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USD", 2), ("c", "EUR", 2)]);
        l.add(&req("a", "100", Kind::Deposit)).unwrap();
        let t = l.transfer(&treq("a", "b", "40")).unwrap();
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[0].amount, "-40.00");
        assert_eq!(t.entries[1].amount, "40.00");
        assert_eq!(t.entries[0].kind, Kind::Transfer);
        assert!(t.entries[0].group_id.is_some());
        assert_eq!(t.entries[0].group_id, t.entries[1].group_id);
        assert_eq!(l.balance("a", None).unwrap().balance, "60.00");
        assert_eq!(l.balance("b", None).unwrap().balance, "40.00");

        assert!(matches!(
            l.transfer(&treq("a", "c", "1")),
            Err(LedgerError::CurrencyMismatch { .. })
        ));
        assert!(matches!(
            l.transfer(&treq("a", "a", "1")),
            Err(LedgerError::SameAccount)
        ));
        assert!(matches!(
            l.transfer(&treq("a", "b", "-1")),
            Err(LedgerError::InvalidSign { .. })
        ));
        assert!(matches!(
            l.transfer(&treq("a", "b", "0")),
            Err(LedgerError::ZeroAmount)
        ));
        assert_eq!(l.balance("a", None).unwrap().balance, "60.00");
    }

    #[test]
    fn transfer_with_ref_is_idempotent_and_conflicts_on_mismatch() {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USD", 2)]);
        let first = l
            .transfer(&TransferRequest {
                reference: Some("mv1".into()),
                group: Some("g1".into()),
                ..treq("a", "b", "5")
            })
            .unwrap();
        assert_eq!(first.entries[0].group_id.as_deref(), Some("g1"));
        let again = l
            .transfer(&TransferRequest {
                reference: Some("mv1".into()),
                ..treq("a", "b", "5")
            })
            .unwrap();
        assert!(again.duplicate);
        assert_eq!(again.entries[0].id, first.entries[0].id);
        let conflict = l.transfer(&TransferRequest {
            reference: Some("mv1".into()),
            ..treq("a", "b", "6")
        });
        assert!(matches!(conflict, Err(LedgerError::RefConflict { .. })));
        assert_eq!(l.balance("b", None).unwrap().balance, "5.00");
    }

    #[test]
    fn reverse_single_entry_inherits_group_and_links_back() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        let e = l
            .add(&AddRequest {
                group: Some("arb:1".into()),
                ..req("w", "-5", Kind::Trade)
            })
            .unwrap()
            .entry;
        let r = l
            .reverse_entry(e.id, Some("oops".into()), Some("claude".into()))
            .unwrap();
        assert_eq!(r.entries.len(), 1);
        let rev = &r.entries[0];
        assert_eq!(rev.kind, Kind::Reversal);
        assert_eq!(rev.amount, "5.00");
        assert_eq!(rev.reverses_id, Some(e.id));
        assert_eq!(rev.group_id.as_deref(), Some("arb:1"));
        assert_eq!(rev.memo.as_deref(), Some("oops"));
        assert_eq!(l.show(e.id).unwrap().entry.reversed_by, Some(rev.id));
        assert_eq!(l.balance("w", None).unwrap().balance, "0.00");

        assert!(matches!(
            l.reverse_entry(e.id, None, None),
            Err(LedgerError::AlreadyReversed(_, _))
        ));
        assert!(matches!(
            l.reverse_entry(rev.id, None, None),
            Err(LedgerError::CannotReverseReversal(_))
        ));
        assert!(matches!(
            l.reverse_entry(999, None, None),
            Err(LedgerError::EntryNotFound(999))
        ));
    }

    #[test]
    fn reversing_a_transfer_leg_reverses_its_sibling() {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USD", 2)]);
        l.add(&req("a", "10", Kind::Deposit)).unwrap();
        let t = l.transfer(&treq("a", "b", "4")).unwrap();
        let r = l.reverse_entry(t.entries[1].id, None, None).unwrap();
        assert_eq!(r.entries.len(), 2);
        assert_eq!(l.balance("a", None).unwrap().balance, "10.00");
        assert_eq!(l.balance("b", None).unwrap().balance, "0.00");
    }

    #[test]
    fn reverse_group_reverses_only_open_entries() {
        let mut l = ledger_with(&[("a", "USDC", 6), ("b", "USD", 2)]);
        l.add(&AddRequest {
            group: Some("arb:9".into()),
            ..req("a", "-45", Kind::Trade)
        })
        .unwrap();
        let leg_b = l
            .add(&AddRequest {
                group: Some("arb:9".into()),
                ..req("b", "-52", Kind::Trade)
            })
            .unwrap()
            .entry;
        l.reverse_entry(leg_b.id, None, None).unwrap();
        let r = l.reverse_group("arb:9", None, None).unwrap();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].account, "a");
        assert!(matches!(
            l.reverse_group("arb:9", None, None),
            Err(LedgerError::NothingToReverse(_))
        ));
        assert!(matches!(
            l.reverse_group("missing", None, None),
            Err(LedgerError::GroupNotFound(_))
        ));
        assert!(matches!(
            l.reverse_group(" ", None, None),
            Err(LedgerError::InvalidGroup)
        ));
    }
}
