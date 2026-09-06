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
}
