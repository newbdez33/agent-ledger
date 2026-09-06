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
    /// Reverse an entry, or every not-yet-reversed entry in a group
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
        /// Unique, case-insensitive name, e.g. poly-usdc
        name: String,
        /// Currency code, stored uppercase (USDC, USD, BTC)
        #[arg(long)]
        currency: String,
        /// Fraction digits the account allows (USDC 6, USD 2, BTC 8); never rounds
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(0..=18))]
        decimals: u32,
        /// Free-text note
        #[arg(long)]
        note: Option<String>,
    },
    /// List accounts
    List,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Account name (case-insensitive)
    pub account: String,
    /// Signed decimal; positive is an inflow
    #[arg(allow_negative_numbers = true)]
    pub amount: String,
    /// deposit (+) | withdrawal (-) | trade (buy -, sell +) | settlement (redeem, resolution,
    /// expiry or funding cash) | fee (-) | adjustment | other
    #[arg(long)]
    pub kind: String,
    /// External id (order id, tx hash); unique per account, makes the call idempotent
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// Free-text note
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
    /// Account money leaves
    pub from: String,
    /// Account money enters (same currency)
    pub to: String,
    /// Positive decimal
    #[arg(allow_negative_numbers = true)]
    pub amount: String,
    /// External id; unique per account, makes the call idempotent
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// Free-text note
    #[arg(long)]
    pub memo: Option<String>,
    /// When it happened (RFC 3339 or YYYY-MM-DD); default now
    #[arg(long)]
    pub ts: Option<String>,
    /// Group id for both legs; default a fresh UUID
    #[arg(long)]
    pub group: Option<String>,
    /// JSON object with structured attributes
    #[arg(long)]
    pub meta: Option<String>,
}

#[derive(Args, Debug)]
pub struct ReverseArgs {
    /// Entry to reverse (or pass --group instead)
    #[arg(required_unless_present = "group", conflicts_with = "group")]
    pub entry_id: Option<i64>,
    /// Reverse every entry in this group that has not been reversed yet
    #[arg(long)]
    pub group: Option<String>,
    /// Free-text note on the reversal
    #[arg(long)]
    pub memo: Option<String>,
}

#[derive(Args, Debug)]
pub struct BalanceArgs {
    /// Account name; every account when omitted
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
    /// Inclusive lower bound on entry ts (RFC 3339 or YYYY-MM-DD, taken as 00:00 UTC)
    #[arg(long)]
    pub since: Option<String>,
    /// Inclusive upper bound on entry ts (RFC 3339 or YYYY-MM-DD, taken as 00:00 UTC, so a
    /// bare date excludes that day)
    #[arg(long)]
    pub until: Option<String>,
    /// Only this kind (any of the nine, including transfer and reversal)
    #[arg(long)]
    pub kind: Option<String>,
    /// Only entries in this group
    #[arg(long)]
    pub group: Option<String>,
}

#[derive(Args, Debug)]
pub struct PnlArgs {
    /// Account name; every account when omitted
    pub account: Option<String>,
    /// Inclusive lower bound on entry ts (RFC 3339 or YYYY-MM-DD, taken as 00:00 UTC)
    #[arg(long)]
    pub since: Option<String>,
    /// Inclusive upper bound on entry ts (RFC 3339 or YYYY-MM-DD, taken as 00:00 UTC, so a
    /// bare date excludes that day; for today pass --since alone)
    #[arg(long)]
    pub until: Option<String>,
    /// total | day | week | month | group | meta:<key>
    #[arg(long, default_value = "total")]
    pub by: String,
    /// JSON file {"<group>": "<amount>", ...} valuing open positions; adds open_value and mtm
    #[arg(long, value_name = "FILE")]
    pub marks: Option<std::path::PathBuf>,
}

#[derive(Args, Debug)]
pub struct ReconcileArgs {
    pub account: String,
    /// Balance you actually observed at the venue or on chain
    #[arg(long, allow_negative_numbers = true)]
    pub observed: String,
    /// Where the observation came from (polygon-rpc, kalshi-api, ...)
    #[arg(long)]
    pub source: Option<String>,
    /// Record the snapshot but do not post an adjustment entry
    #[arg(long)]
    pub no_adjust: bool,
    /// When the balance was observed; the book is compared as of this time.
    /// Default: now, or the latest booked entry if that is later
    #[arg(long)]
    pub ts: Option<String>,
}

#[derive(Args, Debug)]
pub struct SnapshotsArgs {
    pub account: String,
    /// Most recent N snapshots; 0 for all
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
