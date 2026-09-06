use serde::Serialize;

use agent_ledger::model::*;

/// One value per command. Untagged so struct variants serialize as their field map.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Output {
    Account(AccountAddResult),
    Accounts {
        accounts: Vec<Account>,
    },
    Add(AddResult),
    Show(ShowResult),
    Transfer(TransferResult),
    Reverse(ReverseResult),
    Balances {
        accounts: Vec<AccountBalance>,
    },
    Balance(BalanceAt),
    History(History),
    Group(GroupView),
    Pnl {
        accounts: Vec<AccountPnl>,
        /// Whether marks were supplied; drives the table columns, not part of the JSON.
        #[serde(skip)]
        marked: bool,
        /// Whether `--by total` was used, so the one `null` row is the total; not part of the JSON.
        #[serde(skip)]
        total: bool,
    },
    Reconcile(ReconcileResult),
    Snapshots(SnapshotList),
    Import(ImportResult),
    /// Pre-rendered text (CSV) printed verbatim regardless of --json.
    #[serde(skip)]
    Raw(String),
}
