//! Entry kinds and every serializable result type the CLI prints.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::{LedgerError, Result};
use crate::money::format_amount;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deposit,
    Withdrawal,
    Trade,
    Settlement,
    Fee,
    Transfer,
    Adjustment,
    Reversal,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::Deposit,
        Kind::Withdrawal,
        Kind::Trade,
        Kind::Settlement,
        Kind::Fee,
        Kind::Transfer,
        Kind::Adjustment,
        Kind::Reversal,
        Kind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Deposit => "deposit",
            Kind::Withdrawal => "withdrawal",
            Kind::Trade => "trade",
            Kind::Settlement => "settlement",
            Kind::Fee => "fee",
            Kind::Transfer => "transfer",
            Kind::Adjustment => "adjustment",
            Kind::Reversal => "reversal",
            Kind::Other => "other",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        Self::ALL.into_iter().find(|k| k.as_str() == text.trim())
    }

    /// Kinds a caller may write through `add` or `import`.
    pub fn parse_addable(text: &str) -> Result<Kind> {
        match Self::parse(text) {
            Some(Kind::Transfer) | Some(Kind::Reversal) | None => {
                Err(LedgerError::InvalidKind(text.trim().to_string()))
            }
            Some(kind) => Ok(kind),
        }
    }

    /// Zero is always rejected; deposit must be positive; withdrawal and fee negative.
    pub fn check_sign(self, amount: i64, decimals: u32) -> Result<()> {
        if amount == 0 {
            return Err(LedgerError::ZeroAmount);
        }
        let rule = match self {
            Kind::Deposit => Some(("positive", amount > 0)),
            Kind::Withdrawal | Kind::Fee => Some(("negative", amount < 0)),
            _ => None,
        };
        if let Some((expected, ok)) = rule {
            if !ok {
                return Err(LedgerError::InvalidSign {
                    kind: self.as_str().to_string(),
                    expected,
                    amount: format_amount(amount, decimals),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub currency: String,
    pub decimals: u32,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountAddResult {
    pub account: Account,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub id: i64,
    pub account: String,
    pub currency: String,
    pub ts: String,
    pub recorded_at: String,
    pub kind: Kind,
    pub amount: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub memo: Option<String>,
    pub actor: Option<String>,
    /// The `group_id` column; serialized as `group` to match `--group`, `group <id>` and
    /// `pnl --by group`.
    #[serde(rename = "group")]
    pub group_id: Option<String>,
    pub meta: Option<serde_json::Value>,
    pub reverses_id: Option<i64>,
    pub reversed_by: Option<i64>,
    #[serde(skip)]
    pub amount_minor: i64,
    #[serde(skip)]
    pub decimals: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryEntry {
    #[serde(flatten)]
    pub entry: Entry,
    pub balance_after: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    /// `None` on a dry run, which records nothing.
    pub id: Option<i64>,
    pub account: String,
    pub ts: String,
    pub observed: String,
    pub book: String,
    pub diff: String,
    pub adjustment_entry_id: Option<i64>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AddResult {
    pub entry: Entry,
    pub balance: String,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShowResult {
    pub entry: Entry,
    pub balance: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransferResult {
    pub entries: Vec<Entry>,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReverseResult {
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountBalance {
    pub account: String,
    pub currency: String,
    pub balance: String,
    pub entries: i64,
    pub last_ts: Option<String>,
    pub last_reconciled_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BalanceAt {
    pub account: String,
    pub currency: String,
    pub balance: String,
    pub at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct History {
    pub account: String,
    pub currency: String,
    pub entries: Vec<HistoryEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupView {
    pub group: String,
    pub entries: Vec<Entry>,
    pub net: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PnlRow {
    pub bucket: Option<String>,
    pub trades: String,
    pub settlements: String,
    pub fees: String,
    pub adjustments: String,
    pub other: String,
    pub net: String,
    /// Sum of caller-supplied marks for the groups in this row; `None` when no group was marked.
    pub open_value: Option<String>,
    /// `net` plus `open_value`; equals `net` when nothing is marked.
    pub mtm: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountPnl {
    pub account: String,
    pub currency: String,
    pub rows: Vec<PnlRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconcileResult {
    pub snapshot: Snapshot,
    pub adjustment: Option<Entry>,
    /// The same observation (ts, observed, book, source) was already on record; nothing written.
    pub duplicate: bool,
    /// `--dry-run`: observed, book and diff were computed and nothing was written.
    pub dry_run: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotList {
    pub account: String,
    pub snapshots: Vec<Snapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportResult {
    pub imported: usize,
    pub duplicates: usize,
    pub dry_run: bool,
    pub entries: Vec<Entry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips_through_text() {
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("bogus"), None);
    }

    #[test]
    fn transfer_and_reversal_are_not_addable() {
        assert!(matches!(
            Kind::parse_addable("transfer"),
            Err(LedgerError::InvalidKind(_))
        ));
        assert!(matches!(
            Kind::parse_addable("reversal"),
            Err(LedgerError::InvalidKind(_))
        ));
        assert_eq!(Kind::parse_addable("settlement").unwrap(), Kind::Settlement);
    }

    #[test]
    fn sign_rules() {
        assert!(matches!(
            Kind::Deposit.check_sign(0, 2),
            Err(LedgerError::ZeroAmount)
        ));
        assert!(matches!(
            Kind::Deposit.check_sign(-1, 2),
            Err(LedgerError::InvalidSign { .. })
        ));
        assert!(matches!(
            Kind::Fee.check_sign(5, 2),
            Err(LedgerError::InvalidSign { .. })
        ));
        assert!(matches!(
            Kind::Withdrawal.check_sign(5, 2),
            Err(LedgerError::InvalidSign { .. })
        ));
        assert!(Kind::Trade.check_sign(-5, 2).is_ok());
        assert!(Kind::Trade.check_sign(5, 2).is_ok());
        assert!(Kind::Settlement.check_sign(-5, 2).is_ok());
    }

    #[test]
    fn kind_serializes_lowercase_and_ref_is_renamed() {
        assert_eq!(
            serde_json::to_string(&Kind::Settlement).unwrap(),
            "\"settlement\""
        );
        let e = Entry {
            id: 1,
            account: "a".into(),
            currency: "USD".into(),
            ts: "t".into(),
            recorded_at: "t".into(),
            kind: Kind::Trade,
            amount: "-1.00".into(),
            reference: Some("r".into()),
            memo: None,
            actor: None,
            group_id: None,
            meta: None,
            reverses_id: None,
            reversed_by: None,
            amount_minor: -100,
            decimals: 2,
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["ref"], "r");
        assert!(v.get("amount_minor").is_none());
    }
}
