//! Command-line surface: clap definitions only.

pub mod output;
pub mod render;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "ledger",
    version,
    about = "Append-only SQLite ledger for AI agents"
)]
pub struct Cli {
    /// Ledger database file (default: ~/.agent-ledger/ledger.db)
    #[arg(long, global = true, env = "LEDGER_DB", value_name = "PATH")]
    pub db: Option<PathBuf>,
    /// Emit one JSON object on stdout instead of a table
    #[arg(long, global = true)]
    pub json: bool,
    /// Who is writing; recorded on every entry
    #[arg(long, global = true, env = "LEDGER_ACTOR", value_name = "NAME")]
    pub actor: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage accounts
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Record one movement on an account
    Add(AddArgs),
    /// Move money between two same-currency accounts
    Transfer(TransferArgs),
    /// Reverse an entry, or every open entry in a group
    Reverse(ReverseArgs),
    /// Show balances
    Balance(BalanceArgs),
    /// List entries of an account with running balance
    History(HistoryArgs),
    /// Show every entry in a group across accounts
    Group { id: String },
    /// Realized PnL, excluding deposits, withdrawals and transfers
    Pnl(PnlArgs),
    /// Compare book balance with an observed balance and post an adjustment
    Reconcile(ReconcileArgs),
    /// List reconciliation snapshots
    Snapshots(SnapshotsArgs),
    /// Import entries from JSON Lines on stdin
    Import(ImportArgs),
    /// Show one entry
    Show { entry_id: i64 },
    /// Dump an account's entries
    Export(ExportArgs),
}

#[derive(Subcommand, Debug)]
pub enum AccountCommand {
    /// Create an account
    Add {
        name: String,
        #[arg(long)]
        currency: String,
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(0..=18))]
        decimals: u32,
        #[arg(long)]
        note: Option<String>,
    },
    /// List accounts
    List,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    pub account: String,
    /// Signed decimal; positive is an inflow
    #[arg(allow_negative_numbers = true)]
    pub amount: String,
    /// deposit | withdrawal | trade | settlement | fee | adjustment | other
    #[arg(long)]
    pub kind: String,
    /// External id (order id, tx hash); unique per account, makes the call idempotent
    #[arg(long = "ref")]
    pub reference: Option<String>,
    #[arg(long)]
    pub memo: Option<String>,
    /// When it happened (RFC 3339 or YYYY-MM-DD); default now
    #[arg(long)]
    pub ts: Option<String>,
    /// Links the legs of one position across accounts
    #[arg(long)]
    pub group: Option<String>,
    /// JSON object with structured attributes
    #[arg(long)]
    pub meta: Option<String>,
}

#[derive(Args, Debug)]
pub struct TransferArgs {
    pub from: String,
    pub to: String,
    #[arg(allow_negative_numbers = true)]
    pub amount: String,
    #[arg(long = "ref")]
    pub reference: Option<String>,
    #[arg(long)]
    pub memo: Option<String>,
    #[arg(long)]
    pub ts: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
    #[arg(long)]
    pub meta: Option<String>,
}

#[derive(Args, Debug)]
pub struct ReverseArgs {
    #[arg(required_unless_present = "group", conflicts_with = "group")]
    pub entry_id: Option<i64>,
    /// Reverse every open entry in this group
    #[arg(long)]
    pub group: Option<String>,
    #[arg(long)]
    pub memo: Option<String>,
}

#[derive(Args, Debug)]
pub struct BalanceArgs {
    pub account: Option<String>,
    /// Balance as of this time (requires an account)
    #[arg(long, requires = "account")]
    pub at: Option<String>,
}

#[derive(Args, Debug)]
pub struct HistoryArgs {
    pub account: String,
    /// Most recent N entries; 0 for all
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
}

#[derive(Args, Debug)]
pub struct PnlArgs {
    pub account: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    /// total | day | week | month | group | meta:<key>
    #[arg(long, default_value = "total")]
    pub by: String,
}

#[derive(Args, Debug)]
pub struct ReconcileArgs {
    pub account: String,
    /// Balance you actually observed at the venue or on chain
    #[arg(long, allow_negative_numbers = true)]
    pub observed: String,
    #[arg(long)]
    pub source: Option<String>,
    /// Record the snapshot but do not post an adjustment entry
    #[arg(long)]
    pub no_adjust: bool,
    #[arg(long)]
    pub ts: Option<String>,
}

#[derive(Args, Debug)]
pub struct SnapshotsArgs {
    pub account: String,
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// Validate and report without writing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    pub account: String,
    #[arg(long, value_enum)]
    pub format: ExportFormat,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}
