# agent-ledger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `ledger` binary described in the spec: an append-only SQLite ledger with accounts, entries, transfers, reversals, groups, PnL, reconciliation, import/export, JSON and table output, plus the companion Claude Code skill.

**Architecture:** One Rust crate. `src/lib.rs` exposes a `Ledger` struct wrapping a `rusqlite::Connection`; every operation is one `BEGIN IMMEDIATE` transaction implemented in a focused submodule under `src/ledger/`. `src/main.rs` plus `src/cli/` is a thin clap layer that maps arguments to `Ledger` calls, serializes results with serde, and maps `LedgerError` to exit codes. Money is `i64` minor units everywhere inside; strings only at the edges.

**Tech Stack:** Rust 1.97 stable, rusqlite 0.32 (bundled SQLite 3.46: JSON functions and window functions built in), clap 4 (derive, env), rust_decimal 1, chrono 0.4, serde/serde_json, thiserror 2, uuid 1, dirs 5. Dev: assert_cmd 2, predicates 3, tempfile 3.

**Spec:** `docs/specs/2026-09-06-agent-ledger-design.md`

## Global Constraints

- Binary name is `ledger`; crate name is `agent-ledger`; license MIT.
- Amounts are stored as `INTEGER` minor units; `accounts.decimals` is `0..=18`; input with more fractional digits than the account allows is rejected, never rounded; JSON carries amounts as strings.
- Timestamps are stored as RFC 3339 UTC with millisecond precision and a literal `Z`, e.g. `2026-09-06T03:12:45.117Z`; caller timestamps are normalized to that format; a bare `YYYY-MM-DD` means UTC midnight.
- `entries` and `snapshots` are append-only, enforced by `BEFORE UPDATE` / `BEFORE DELETE` triggers that `RAISE(ABORT, ...)`.
- Kinds: `deposit` (positive), `withdrawal` (negative), `trade`, `settlement`, `fee` (negative), `transfer`, `adjustment`, `reversal`, `other`. Zero amounts are rejected. `transfer` and `reversal` are never written by `add` or `import`.
- `(account_id, ref)` is unique when `ref` is not null; replaying the same ref with the same kind and amount is a success with `duplicate: true`; a different kind or amount is `ref_conflict`.
- DB path: `--db`, else `LEDGER_DB`, else `~/.agent-ledger/ledger.db`; the directory is created; opening migrates.
- Exit codes: 0 success, 1 usage/IO/database error, 2 domain error. Errors go to stderr; in `--json` mode stderr carries `{"error":{"code","message"[,"line"]}}` and stdout stays empty.
- Commit after every task with a conventional-commit message. No `Co-Authored-By` trailer.
- Run `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before each commit; both must be clean.

---

## File structure

| file | responsibility |
|---|---|
| `Cargo.toml`, `Cargo.lock` | crate manifest; lockfile is committed (binary crate) |
| `src/lib.rs` | module declarations and re-exports (`Ledger`, `LedgerError`, `model::*`) |
| `src/error.rs` | `LedgerError` enum, `code()`, `exit_code()`, `line()`, `Result<T>` alias |
| `src/money.rs` | `parse_amount`, `format_amount`, `format_wide`, `AmountError` — pure, no DB |
| `src/time.rs` | `now()`, `normalize()` — storage timestamp format |
| `src/model.rs` | `Kind` and every serializable result type the CLI prints |
| `src/db.rs` | `open`, `open_in_memory`, pragmas, schema v1, migration |
| `src/ledger/mod.rs` | `Ledger` struct, shared row helpers (`account_by_name`, `write_entry`, `load_entry`, `balance_minor`, validators) |
| `src/ledger/accounts.rs` | `add_account`, `list_accounts` |
| `src/ledger/entries.rs` | `add`, `show`, `transfer`, `reverse_entry`, `reverse_group` |
| `src/ledger/reports.rs` | `balances`, `balance`, `history`, `export`, `group` |
| `src/ledger/pnl.rs` | `PnlBucket`, `pnl` |
| `src/ledger/reconcile.rs` | `reconcile`, `snapshots` |
| `src/ledger/import.rs` | `import` from JSON Lines |
| `src/cli/mod.rs` | clap derive structs |
| `src/cli/output.rs` | `Output` enum (untagged serde) |
| `src/cli/render.rs` | text tables and CSV |
| `src/main.rs` | parse, dispatch, print, exit code |
| `tests/cli.rs` | end-to-end tests against the built binary |
| `skill/ledger/SKILL.md` | companion skill |
| `Makefile` | `install` / `uninstall` |

---

### Task 1: Crate scaffold, errors, money

**Files:**
- Create: `Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/money.rs`, `src/main.rs` (placeholder so the crate builds)

**Interfaces:**
- Produces: `LedgerError` (all variants below), `type Result<T> = std::result::Result<T, LedgerError>`, `money::parse_amount(&str, u32) -> Result<i64, AmountError>`, `money::format_amount(i64, u32) -> String`, `money::format_wide(i128, u32) -> String`, `AmountError::{Invalid, Precision{scale}}`.

- [ ] **Step 1: Write Cargo.toml and the module skeleton**

`Cargo.toml`:

```toml
[package]
name = "agent-ledger"
version = "0.1.0"
edition = "2021"
license = "MIT"
description = "Append-only SQLite ledger CLI for AI agents"
repository = "https://github.com/newbdez33/agent-ledger"
publish = false

[[bin]]
name = "ledger"
path = "src/main.rs"

[dependencies]
rusqlite = { version = "0.32", features = ["bundled"] }
clap = { version = "4", features = ["derive", "env"] }
rust_decimal = "1"
chrono = "0.4"
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
thiserror = "2"
uuid = { version = "1", features = ["v4"] }
dirs = "5"

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

`src/lib.rs`:

```rust
pub mod error;
pub mod money;

pub use error::{LedgerError, Result};
```

`src/main.rs` (placeholder, replaced in Task 11):

```rust
fn main() {}
```

- [ ] **Step 2: Write the failing money tests**

`src/money.rs`:

```rust
//! Exact money: decimal text at the edges, i64 minor units inside.

#[derive(Debug, PartialEq, Eq)]
pub enum AmountError {
    Invalid,
    Precision { scale: u32 },
}

pub fn parse_amount(text: &str, decimals: u32) -> Result<i64, AmountError> {
    todo!()
}

pub fn format_amount(minor: i64, decimals: u32) -> String {
    todo!()
}

pub fn format_wide(minor: i128, decimals: u32) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whole_number_into_minor_units() {
        assert_eq!(parse_amount("100", 6), Ok(100_000_000));
    }

    #[test]
    fn parses_negative_decimal() {
        assert_eq!(parse_amount("-25.5", 6), Ok(-25_500_000));
    }

    #[test]
    fn accepts_leading_plus_and_trailing_zeros() {
        assert_eq!(parse_amount("+1.500000", 2), Ok(150));
        assert_eq!(parse_amount(" 7 ", 0), Ok(7));
    }

    #[test]
    fn rejects_excess_precision_without_rounding() {
        assert_eq!(parse_amount("1.2345678", 6), Err(AmountError::Precision { scale: 7 }));
        assert_eq!(parse_amount("0.001", 2), Err(AmountError::Precision { scale: 3 }));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_amount("abc", 2), Err(AmountError::Invalid));
        assert_eq!(parse_amount("1e5", 2), Err(AmountError::Invalid));
        assert_eq!(parse_amount("", 2), Err(AmountError::Invalid));
    }

    #[test]
    fn formats_with_fixed_decimals() {
        assert_eq!(format_amount(-25_500_000, 6), "-25.500000");
        assert_eq!(format_amount(150, 2), "1.50");
        assert_eq!(format_amount(-5, 2), "-0.05");
        assert_eq!(format_amount(7, 0), "7");
        assert_eq!(format_amount(0, 6), "0.000000");
    }

    #[test]
    fn round_trips() {
        for s in ["0.000001", "-99999.123456", "1", "-0.5"] {
            let m = parse_amount(s, 6).unwrap();
            assert_eq!(parse_amount(&format_amount(m, 6), 6).unwrap(), m);
        }
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test money`
Expected: panics with `not yet implemented`.

- [ ] **Step 4: Implement money**

Replace the three `todo!()` bodies:

```rust
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::str::FromStr;

pub fn parse_amount(text: &str, decimals: u32) -> Result<i64, AmountError> {
    let trimmed = text.trim();
    let unsigned = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let value = Decimal::from_str(unsigned)
        .map_err(|_| AmountError::Invalid)?
        .normalize();
    if value.scale() > decimals {
        return Err(AmountError::Precision { scale: value.scale() });
    }
    let factor = Decimal::from(10i64.pow(decimals));
    let scaled = value.checked_mul(factor).ok_or(AmountError::Invalid)?;
    scaled.to_i64().ok_or(AmountError::Invalid)
}

pub fn format_amount(minor: i64, decimals: u32) -> String {
    format_wide(minor as i128, decimals)
}

pub fn format_wide(minor: i128, decimals: u32) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    if decimals == 0 {
        return format!("{sign}{abs}");
    }
    let base = 10u128.pow(decimals);
    format!("{sign}{}.{:0width$}", abs / base, abs % base, width = decimals as usize)
}
```

- [ ] **Step 5: Write error.rs**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("account '{0}' not found")]
    AccountNotFound(String),
    #[error("account '{0}' already exists")]
    AccountExists(String),
    #[error("account name must not be empty")]
    InvalidAccountName,
    #[error("currency must not be empty")]
    InvalidCurrency,
    #[error("from and to must be different accounts")]
    SameAccount,
    #[error("entry {0} not found")]
    EntryNotFound(i64),
    #[error("group '{0}' not found")]
    GroupNotFound(String),
    #[error("currency mismatch: '{from}' is {from_currency}, '{to}' is {to_currency}")]
    CurrencyMismatch { from: String, from_currency: String, to: String, to_currency: String },
    #[error("amount {amount} has {scale} decimals; account '{account}' allows {decimals}")]
    PrecisionExceeded { amount: String, scale: u32, account: String, decimals: u32 },
    #[error("invalid amount '{0}'")]
    InvalidAmount(String),
    #[error("amount must not be zero")]
    ZeroAmount,
    #[error("{kind} must be {expected}, got {amount}")]
    InvalidSign { kind: String, expected: &'static str, amount: String },
    #[error("invalid kind '{0}'")]
    InvalidKind(String),
    #[error("invalid timestamp '{0}' (expected RFC 3339 or YYYY-MM-DD)")]
    InvalidTimestamp(String),
    #[error("group id must not be empty")]
    InvalidGroup,
    #[error("meta must be a JSON object: {0}")]
    InvalidMeta(String),
    #[error("invalid pnl bucket '{0}' (expected total, day, week, month, group or meta:<key>)")]
    InvalidBucket(String),
    #[error("ref '{reference}' on '{account}' already exists as entry {existing_id} ({existing_kind} {existing_amount})")]
    RefConflict { account: String, reference: String, existing_id: i64, existing_kind: String, existing_amount: String },
    #[error("entry {0} is already reversed by entry {1}")]
    AlreadyReversed(i64, i64),
    #[error("entry {0} is a reversal; add the original again instead of reversing it")]
    CannotReverseReversal(i64),
    #[error("nothing to reverse in group '{0}'")]
    NothingToReverse(String),
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("line {line}: {source}")]
    Import { line: usize, #[source] source: Box<LedgerError> },
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl LedgerError {
    pub fn code(&self) -> &'static str {
        use LedgerError::*;
        match self {
            AccountNotFound(_) => "account_not_found",
            AccountExists(_) => "account_exists",
            InvalidAccountName => "invalid_account_name",
            InvalidCurrency => "invalid_currency",
            SameAccount => "same_account",
            EntryNotFound(_) => "entry_not_found",
            GroupNotFound(_) => "group_not_found",
            CurrencyMismatch { .. } => "currency_mismatch",
            PrecisionExceeded { .. } => "precision_exceeded",
            InvalidAmount(_) => "invalid_amount",
            ZeroAmount => "zero_amount",
            InvalidSign { .. } => "invalid_sign",
            InvalidKind(_) => "invalid_kind",
            InvalidTimestamp(_) => "invalid_timestamp",
            InvalidGroup => "invalid_group",
            InvalidMeta(_) => "invalid_meta",
            InvalidBucket(_) => "invalid_bucket",
            RefConflict { .. } => "ref_conflict",
            AlreadyReversed(..) => "already_reversed",
            CannotReverseReversal(_) => "cannot_reverse_reversal",
            NothingToReverse(_) => "nothing_to_reverse",
            InvalidJson(_) => "invalid_json",
            Import { source, .. } => source.code(),
            Db(_) => "database_error",
            Io(_) => "io_error",
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            LedgerError::Db(_) | LedgerError::Io(_) => 1,
            LedgerError::Import { source, .. } => source.exit_code(),
            _ => 2,
        }
    }

    pub fn line(&self) -> Option<usize> {
        match self {
            LedgerError::Import { line, .. } => Some(*line),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, LedgerError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_errors_exit_2_and_infra_errors_exit_1() {
        assert_eq!(LedgerError::ZeroAmount.exit_code(), 2);
        assert_eq!(LedgerError::Io(std::io::Error::other("x")).exit_code(), 1);
    }

    #[test]
    fn import_wrapper_delegates_code_and_exposes_line() {
        let e = LedgerError::Import { line: 3, source: Box::new(LedgerError::InvalidGroup) };
        assert_eq!(e.code(), "invalid_group");
        assert_eq!(e.line(), Some(3));
        assert_eq!(e.exit_code(), 2);
        assert_eq!(e.to_string(), "line 3: group id must not be empty");
    }
}
```

- [ ] **Step 6: Run all tests, fmt, clippy**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all money and error tests PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/main.rs src/error.rs src/money.rs
git commit -m "feat: crate scaffold with error codes and exact money parsing"
```

---

### Task 2: Time and model

**Files:**
- Create: `src/time.rs`, `src/model.rs`
- Modify: `src/lib.rs` (add `pub mod time; pub mod model;` and `pub use model::*;`)

**Interfaces:**
- Consumes: `money::format_amount`, `LedgerError`.
- Produces: `time::now() -> String`, `time::normalize(&str) -> Option<String>`; `Kind` with `as_str`, `parse`, `parse_addable`, `check_sign(amount: i64, decimals: u32) -> Result<()>`; result structs `Account`, `Entry`, `HistoryEntry`, `Snapshot`, `AddResult`, `ShowResult`, `TransferResult`, `ReverseResult`, `AccountBalance`, `BalanceAt`, `History`, `GroupView`, `PnlRow`, `AccountPnl`, `ReconcileResult`, `SnapshotList`, `ImportResult`.

- [ ] **Step 1: Write failing time tests**

`src/time.rs`:

```rust
//! Storage timestamp format: RFC 3339 UTC with milliseconds and a literal Z.

pub fn now() -> String {
    todo!()
}

/// Accepts RFC 3339 with any offset, or a bare `YYYY-MM-DD` (UTC midnight).
pub fn normalize(text: &str) -> Option<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_offset_to_utc_millis() {
        assert_eq!(normalize("2026-09-06T12:00:00+09:00").unwrap(), "2026-09-06T03:00:00.000Z");
    }

    #[test]
    fn accepts_date_only_as_utc_midnight() {
        assert_eq!(normalize(" 2026-09-01 ").unwrap(), "2026-09-01T00:00:00.000Z");
    }

    #[test]
    fn keeps_millis() {
        assert_eq!(normalize("2026-09-06T03:12:45.117Z").unwrap(), "2026-09-06T03:12:45.117Z");
    }

    #[test]
    fn rejects_garbage() {
        assert!(normalize("yesterday").is_none());
        assert!(normalize("2026-13-01").is_none());
    }

    #[test]
    fn now_is_storage_format() {
        let n = now();
        assert_eq!(n.len(), 24);
        assert!(n.ends_with('Z'));
        assert_eq!(normalize(&n).unwrap(), n);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test time`
Expected: `not yet implemented` panics.

- [ ] **Step 3: Implement time**

```rust
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};

pub fn now() -> String {
    to_storage(Utc::now())
}

pub fn normalize(text: &str) -> Option<String> {
    let t = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Some(to_storage(dt.with_timezone(&Utc)));
    }
    let date = NaiveDate::parse_from_str(t, "%Y-%m-%d").ok()?;
    Some(to_storage(date.and_hms_opt(0, 0, 0)?.and_utc()))
}

fn to_storage(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}
```

- [ ] **Step 4: Write failing model tests**

`src/model.rs`:

```rust
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
        Kind::Deposit, Kind::Withdrawal, Kind::Trade, Kind::Settlement, Kind::Fee,
        Kind::Transfer, Kind::Adjustment, Kind::Reversal, Kind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        todo!()
    }

    pub fn parse(text: &str) -> Option<Kind> {
        todo!()
    }

    /// Kinds a caller may write through `add` or `import`.
    pub fn parse_addable(text: &str) -> Result<Kind> {
        todo!()
    }

    /// Zero is always rejected; deposit must be positive; withdrawal and fee negative.
    pub fn check_sign(self, amount: i64, decimals: u32) -> Result<()> {
        todo!()
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
    pub id: i64,
    pub account: String,
    pub ts: String,
    pub observed: String,
    pub book: String,
    pub diff: String,
    pub adjustment_entry_id: Option<i64>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AddResult { pub entry: Entry, pub balance: String, pub duplicate: bool }

#[derive(Clone, Debug, Serialize)]
pub struct ShowResult { pub entry: Entry, pub balance: String }

#[derive(Clone, Debug, Serialize)]
pub struct TransferResult { pub entries: Vec<Entry>, pub duplicate: bool }

#[derive(Clone, Debug, Serialize)]
pub struct ReverseResult { pub entries: Vec<Entry> }

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
pub struct BalanceAt { pub account: String, pub currency: String, pub balance: String, pub at: Option<String> }

#[derive(Clone, Debug, Serialize)]
pub struct History { pub account: String, pub currency: String, pub entries: Vec<HistoryEntry> }

#[derive(Clone, Debug, Serialize)]
pub struct GroupView { pub group: String, pub entries: Vec<Entry>, pub net: BTreeMap<String, String> }

#[derive(Clone, Debug, Serialize)]
pub struct PnlRow {
    pub bucket: Option<String>,
    pub trades: String,
    pub settlements: String,
    pub fees: String,
    pub adjustments: String,
    pub other: String,
    pub net: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountPnl { pub account: String, pub currency: String, pub rows: Vec<PnlRow> }

#[derive(Clone, Debug, Serialize)]
pub struct ReconcileResult { pub snapshot: Snapshot, pub adjustment: Option<Entry> }

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotList { pub account: String, pub snapshots: Vec<Snapshot> }

#[derive(Clone, Debug, Serialize)]
pub struct ImportResult { pub imported: usize, pub duplicates: usize, pub dry_run: bool, pub entries: Vec<Entry> }

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
        assert!(matches!(Kind::parse_addable("transfer"), Err(LedgerError::InvalidKind(_))));
        assert!(matches!(Kind::parse_addable("reversal"), Err(LedgerError::InvalidKind(_))));
        assert_eq!(Kind::parse_addable("settlement").unwrap(), Kind::Settlement);
    }

    #[test]
    fn sign_rules() {
        assert!(matches!(Kind::Deposit.check_sign(0, 2), Err(LedgerError::ZeroAmount)));
        assert!(matches!(Kind::Deposit.check_sign(-1, 2), Err(LedgerError::InvalidSign { .. })));
        assert!(matches!(Kind::Fee.check_sign(5, 2), Err(LedgerError::InvalidSign { .. })));
        assert!(matches!(Kind::Withdrawal.check_sign(5, 2), Err(LedgerError::InvalidSign { .. })));
        assert!(Kind::Trade.check_sign(-5, 2).is_ok());
        assert!(Kind::Trade.check_sign(5, 2).is_ok());
        assert!(Kind::Settlement.check_sign(-5, 2).is_ok());
    }

    #[test]
    fn kind_serializes_lowercase_and_ref_is_renamed() {
        assert_eq!(serde_json::to_string(&Kind::Settlement).unwrap(), "\"settlement\"");
        let e = Entry {
            id: 1, account: "a".into(), currency: "USD".into(), ts: "t".into(), recorded_at: "t".into(),
            kind: Kind::Trade, amount: "-1.00".into(), reference: Some("r".into()), memo: None, actor: None,
            group_id: None, meta: None, reverses_id: None, reversed_by: None, amount_minor: -100, decimals: 2,
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["ref"], "r");
        assert!(v.get("amount_minor").is_none());
    }
}
```

- [ ] **Step 5: Run to verify failure, then implement Kind**

Run: `cargo test model` — expect `not yet implemented`. Then fill in:

```rust
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

    pub fn parse_addable(text: &str) -> Result<Kind> {
        match Self::parse(text) {
            Some(Kind::Transfer) | Some(Kind::Reversal) | None => {
                Err(LedgerError::InvalidKind(text.trim().to_string()))
            }
            Some(kind) => Ok(kind),
        }
    }

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
```

Add to `src/lib.rs`: `pub mod model; pub mod time;` and `pub use model::*;`.

- [ ] **Step 6: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add src/lib.rs src/time.rs src/model.rs
git commit -m "feat: storage timestamps, entry kinds with sign rules, result types"
```

---

### Task 3: Database schema and migration

**Files:**
- Create: `src/db.rs`
- Modify: `src/lib.rs` (add `pub mod db;`)

**Interfaces:**
- Produces: `db::open(&Path) -> Result<Connection>`, `db::open_in_memory() -> Result<Connection>`, `db::SCHEMA_VERSION: i64 = 1`.

- [ ] **Step 1: Write failing tests**

`src/db.rs`:

```rust
//! SQLite connection setup and schema migrations.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::Result;

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = r#"
CREATE TABLE accounts (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
  currency    TEXT NOT NULL,
  decimals    INTEGER NOT NULL CHECK (decimals BETWEEN 0 AND 18),
  note        TEXT,
  created_at  TEXT NOT NULL
);

CREATE TABLE entries (
  id           INTEGER PRIMARY KEY,
  account_id   INTEGER NOT NULL REFERENCES accounts(id),
  ts           TEXT NOT NULL,
  recorded_at  TEXT NOT NULL,
  kind         TEXT NOT NULL CHECK (kind IN
                 ('deposit','withdrawal','trade','settlement','fee',
                  'transfer','adjustment','reversal','other')),
  amount       INTEGER NOT NULL CHECK (amount <> 0),
  ref          TEXT,
  memo         TEXT,
  actor        TEXT,
  group_id     TEXT,
  meta         TEXT CHECK (meta IS NULL OR json_type(meta) = 'object'),
  reverses_id  INTEGER REFERENCES entries(id)
);
CREATE UNIQUE INDEX entries_account_ref ON entries(account_id, ref) WHERE ref IS NOT NULL;
CREATE UNIQUE INDEX entries_reverses    ON entries(reverses_id)     WHERE reverses_id IS NOT NULL;
CREATE INDEX        entries_account_ts  ON entries(account_id, ts, id);
CREATE INDEX        entries_group       ON entries(group_id)        WHERE group_id IS NOT NULL;

CREATE TRIGGER entries_no_update BEFORE UPDATE ON entries
  BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;
CREATE TRIGGER entries_no_delete BEFORE DELETE ON entries
  BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;

CREATE TABLE snapshots (
  id                   INTEGER PRIMARY KEY,
  account_id           INTEGER NOT NULL REFERENCES accounts(id),
  ts                   TEXT NOT NULL,
  observed             INTEGER NOT NULL,
  book                 INTEGER NOT NULL,
  diff                 INTEGER NOT NULL,
  adjustment_entry_id  INTEGER REFERENCES entries(id),
  source               TEXT
);
CREATE TRIGGER snapshots_no_update BEFORE UPDATE ON snapshots
  BEGIN SELECT RAISE(ABORT, 'ledger snapshots are append-only'); END;
CREATE TRIGGER snapshots_no_delete BEFORE DELETE ON snapshots
  BEGIN SELECT RAISE(ABORT, 'ledger snapshots are append-only'); END;
"#;

pub fn open(path: &Path) -> Result<Connection> {
    todo!()
}

pub fn open_in_memory() -> Result<Connection> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = open_in_memory().unwrap();
        c.execute_batch(
            "INSERT INTO accounts (name, currency, decimals, created_at) VALUES ('a', 'USD', 2, 't');
             INSERT INTO accounts (name, currency, decimals, created_at) VALUES ('b', 'USD', 2, 't');
             INSERT INTO entries (account_id, ts, recorded_at, kind, amount, ref) VALUES (1, 't', 't', 'deposit', 100, 'r1');
             INSERT INTO snapshots (account_id, ts, observed, book, diff) VALUES (1, 't', 100, 100, 0);",
        )
        .unwrap();
        c
    }

    #[test]
    fn creates_schema_and_records_version() {
        let c = open_in_memory().unwrap();
        let v: String = c
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('accounts','entries','snapshots')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn open_creates_parent_dir_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("ledger.db");
        open(&p).unwrap();
        let c = open(&p).unwrap();
        assert!(p.exists());
        let mode: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        let fk: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn entries_are_append_only() {
        let c = seeded();
        let up = c.execute("UPDATE entries SET amount = 1", []).unwrap_err();
        assert!(up.to_string().contains("append-only"), "{up}");
        let del = c.execute("DELETE FROM entries", []).unwrap_err();
        assert!(del.to_string().contains("append-only"), "{del}");
    }

    #[test]
    fn snapshots_are_append_only() {
        let c = seeded();
        assert!(c.execute("UPDATE snapshots SET diff = 1", []).unwrap_err().to_string().contains("append-only"));
        assert!(c.execute("DELETE FROM snapshots", []).unwrap_err().to_string().contains("append-only"));
    }

    #[test]
    fn meta_must_be_a_json_object() {
        let c = seeded();
        let bad = c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, meta) VALUES (1,'t','t','trade',-1,'[1]')",
            [],
        );
        assert!(bad.is_err());
        c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, meta) VALUES (1,'t','t','trade',-1,'{\"a\":1}')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn ref_is_unique_per_account_only() {
        let c = seeded();
        let dup = c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, ref) VALUES (1,'t','t','deposit',5,'r1')",
            [],
        );
        assert!(dup.is_err());
        c.execute(
            "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, ref) VALUES (2,'t','t','deposit',5,'r1')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn zero_amount_and_bad_kind_rejected_by_schema() {
        let c = seeded();
        assert!(c.execute("INSERT INTO entries (account_id, ts, recorded_at, kind, amount) VALUES (1,'t','t','trade',0)", []).is_err());
        assert!(c.execute("INSERT INTO entries (account_id, ts, recorded_at, kind, amount) VALUES (1,'t','t','pnl',1)", []).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test db::`
Expected: `not yet implemented`.

- [ ] **Step 3: Implement open and migrate**

```rust
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
    let version: i64 = conn.query_row(
        "SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'), 0)",
        [],
        |r| r.get(0),
    )?;
    if version < 1 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V1} INSERT INTO meta (key, value) VALUES ('schema_version', '1'); COMMIT;"
        ))?;
    }
    Ok(())
}
```

Add `pub mod db;` to `src/lib.rs`.

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add src/lib.rs src/db.rs
git commit -m "feat: sqlite schema v1 with append-only triggers and migration"
```

---

### Task 4: Ledger core helpers and accounts

**Files:**
- Create: `src/ledger/mod.rs`, `src/ledger/accounts.rs`
- Modify: `src/lib.rs` (add `pub mod ledger; pub use ledger::Ledger;`)

**Interfaces:**
- Consumes: `db::open*`, `money`, `time`, `model::Kind`, `model::Entry`, `model::Account`.
- Produces (crate-internal, used by every later task): `Ledger { conn }`, `Ledger::write_tx(&mut self) -> Result<Transaction>`, `AccountRow { id, name, currency, decimals }`, `account_by_name(&Connection, &str) -> Result<AccountRow>`, `all_accounts(&Connection) -> Result<Vec<AccountRow>>`, `parse_account_amount(&str, &AccountRow) -> Result<i64>`, `resolve_ts(Option<&str>) -> Result<String>`, `validate_group(Option<&str>) -> Result<Option<String>>`, `validate_meta(Option<&str>) -> Result<Option<String>>`, `meta_value_to_storage(&Value) -> Result<String>`, `clean_ref(Option<&str>) -> Option<String>`, `NewEntry`, `Written::{Inserted(i64), Duplicate(i64)}`, `write_entry(&Connection, &NewEntry) -> Result<Written>`, `ENTRY_SELECT: &str`, `entry_from_row(&Row) -> rusqlite::Result<Entry>`, `load_entry(&Connection, i64) -> Result<Entry>`, `balance_minor(&Connection, i64, Option<&str>) -> Result<i64>`.
- Produces (public): `Ledger::open(&Path)`, `Ledger::open_in_memory()`, `Ledger::add_account(&mut self, name, currency, decimals, note) -> Result<Account>`, `Ledger::list_accounts(&self) -> Result<Vec<Account>>`.

- [ ] **Step 1: Write `src/ledger/mod.rs`**

```rust
//! The `Ledger` type and row-level helpers shared by every operation.

mod accounts;

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use crate::db;
use crate::error::{LedgerError, Result};
use crate::model::{Entry, Kind};
use crate::money::{self, AmountError};
use crate::time;

pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self { conn: db::open(path)? })
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self { conn: db::open_in_memory()? })
    }

    pub(crate) fn write_tx(&mut self) -> Result<Transaction<'_>> {
        Ok(self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AccountRow {
    pub id: i64,
    pub name: String,
    pub currency: String,
    pub decimals: u32,
}

fn account_row(r: &Row<'_>) -> rusqlite::Result<AccountRow> {
    Ok(AccountRow {
        id: r.get(0)?,
        name: r.get(1)?,
        currency: r.get(2)?,
        decimals: r.get::<_, i64>(3)? as u32,
    })
}

pub(crate) fn account_by_name(conn: &Connection, name: &str) -> Result<AccountRow> {
    let name = name.trim();
    conn.query_row(
        "SELECT id, name, currency, decimals FROM accounts WHERE name = ?1 COLLATE NOCASE",
        params![name],
        account_row,
    )
    .optional()?
    .ok_or_else(|| LedgerError::AccountNotFound(name.to_string()))
}

pub(crate) fn all_accounts(conn: &Connection) -> Result<Vec<AccountRow>> {
    let mut stmt = conn.prepare("SELECT id, name, currency, decimals FROM accounts ORDER BY name COLLATE NOCASE")?;
    let rows = stmt.query_map([], account_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn parse_account_amount(text: &str, account: &AccountRow) -> Result<i64> {
    money::parse_amount(text, account.decimals).map_err(|e| match e {
        AmountError::Invalid => LedgerError::InvalidAmount(text.trim().to_string()),
        AmountError::Precision { scale } => LedgerError::PrecisionExceeded {
            amount: text.trim().to_string(),
            scale,
            account: account.name.clone(),
            decimals: account.decimals,
        },
    })
}

pub(crate) fn resolve_ts(ts: Option<&str>) -> Result<String> {
    match ts {
        None => Ok(time::now()),
        Some(t) => time::normalize(t).ok_or_else(|| LedgerError::InvalidTimestamp(t.trim().to_string())),
    }
}

pub(crate) fn normalize_opt_ts(ts: Option<&str>) -> Result<Option<String>> {
    ts.map(|t| resolve_ts(Some(t))).transpose()
}

pub(crate) fn validate_group(group: Option<&str>) -> Result<Option<String>> {
    match group.map(str::trim) {
        None => Ok(None),
        Some("") => Err(LedgerError::InvalidGroup),
        Some(g) => Ok(Some(g.to_string())),
    }
}

pub(crate) fn validate_meta(meta: Option<&str>) -> Result<Option<String>> {
    match meta {
        None => Ok(None),
        Some(text) => {
            let value: serde_json::Value =
                serde_json::from_str(text).map_err(|e| LedgerError::InvalidMeta(e.to_string()))?;
            meta_value_to_storage(&value).map(Some)
        }
    }
}

pub(crate) fn meta_value_to_storage(value: &serde_json::Value) -> Result<String> {
    if value.is_object() {
        Ok(value.to_string())
    } else {
        Err(LedgerError::InvalidMeta(format!("got {value}")))
    }
}

pub(crate) fn clean_ref(reference: Option<&str>) -> Option<String> {
    reference.map(str::trim).filter(|r| !r.is_empty()).map(str::to_string)
}

pub(crate) struct NewEntry<'a> {
    pub account: &'a AccountRow,
    pub ts: String,
    pub kind: Kind,
    pub amount: i64,
    pub reference: Option<String>,
    pub memo: Option<String>,
    pub actor: Option<String>,
    pub group_id: Option<String>,
    pub meta: Option<String>,
    pub reverses_id: Option<i64>,
}

pub(crate) enum Written {
    Inserted(i64),
    Duplicate(i64),
}

pub(crate) struct ExistingRef {
    pub id: i64,
    pub kind: String,
    pub amount: i64,
}

pub(crate) fn existing_ref(conn: &Connection, account_id: i64, reference: &str) -> Result<Option<ExistingRef>> {
    Ok(conn
        .query_row(
            "SELECT id, kind, amount FROM entries WHERE account_id = ?1 AND ref = ?2",
            params![account_id, reference],
            |r| Ok(ExistingRef { id: r.get(0)?, kind: r.get(1)?, amount: r.get(2)? }),
        )
        .optional()?)
}

/// Insert one entry, honouring sign rules and ref idempotency.
pub(crate) fn write_entry(conn: &Connection, e: &NewEntry<'_>) -> Result<Written> {
    e.kind.check_sign(e.amount, e.account.decimals)?;
    if let Some(reference) = &e.reference {
        if let Some(existing) = existing_ref(conn, e.account.id, reference)? {
            if existing.kind == e.kind.as_str() && existing.amount == e.amount {
                return Ok(Written::Duplicate(existing.id));
            }
            return Err(LedgerError::RefConflict {
                account: e.account.name.clone(),
                reference: reference.clone(),
                existing_id: existing.id,
                existing_kind: existing.kind,
                existing_amount: money::format_amount(existing.amount, e.account.decimals),
            });
        }
    }
    conn.execute(
        "INSERT INTO entries (account_id, ts, recorded_at, kind, amount, ref, memo, actor, group_id, meta, reverses_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            e.account.id, e.ts, time::now(), e.kind.as_str(), e.amount, e.reference, e.memo,
            e.actor, e.group_id, e.meta, e.reverses_id
        ],
    )?;
    Ok(Written::Inserted(conn.last_insert_rowid()))
}

/// Column order: 0 id, 1 account, 2 currency, 3 decimals, 4 ts, 5 recorded_at, 6 kind,
/// 7 amount, 8 ref, 9 memo, 10 actor, 11 group_id, 12 meta, 13 reverses_id, 14 reversed_by.
pub(crate) const ENTRY_SELECT: &str = "SELECT e.id, a.name, a.currency, a.decimals, e.ts, e.recorded_at, e.kind, e.amount, \
     e.ref, e.memo, e.actor, e.group_id, e.meta, e.reverses_id, \
     (SELECT r.id FROM entries r WHERE r.reverses_id = e.id) \
     FROM entries e JOIN accounts a ON a.id = e.account_id";

pub(crate) fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<Entry> {
    let decimals = row.get::<_, i64>(3)? as u32;
    let amount_minor: i64 = row.get(7)?;
    let kind_text: String = row.get(6)?;
    let kind = Kind::parse(&kind_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("unknown kind {kind_text}").into(),
        )
    })?;
    let meta: Option<String> = row.get(12)?;
    Ok(Entry {
        id: row.get(0)?,
        account: row.get(1)?,
        currency: row.get(2)?,
        ts: row.get(4)?,
        recorded_at: row.get(5)?,
        kind,
        amount: money::format_amount(amount_minor, decimals),
        reference: row.get(8)?,
        memo: row.get(9)?,
        actor: row.get(10)?,
        group_id: row.get(11)?,
        meta: meta.and_then(|m| serde_json::from_str(&m).ok()),
        reverses_id: row.get(13)?,
        reversed_by: row.get(14)?,
        amount_minor,
        decimals,
    })
}

pub(crate) fn load_entry(conn: &Connection, id: i64) -> Result<Entry> {
    conn.query_row(&format!("{ENTRY_SELECT} WHERE e.id = ?1"), params![id], entry_from_row)
        .optional()?
        .ok_or(LedgerError::EntryNotFound(id))
}

pub(crate) fn balance_minor(conn: &Connection, account_id: i64, at: Option<&str>) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(amount), 0) FROM entries WHERE account_id = ?1 AND (?2 IS NULL OR ts <= ?2)",
        params![account_id, at],
        |r| r.get(0),
    )?)
}
```

- [ ] **Step 2: Write failing account tests**

`src/ledger/accounts.rs`:

```rust
use rusqlite::{params, Connection, Row};

use super::{account_by_name, Ledger};
use crate::error::{LedgerError, Result};
use crate::model::Account;
use crate::time;

fn account_from_row(r: &Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        name: r.get(1)?,
        currency: r.get(2)?,
        decimals: r.get::<_, i64>(3)? as u32,
        note: r.get(4)?,
        created_at: r.get(5)?,
    })
}

const ACCOUNT_SELECT: &str = "SELECT id, name, currency, decimals, note, created_at FROM accounts";

fn load_account(conn: &Connection, id: i64) -> Result<Account> {
    Ok(conn.query_row(&format!("{ACCOUNT_SELECT} WHERE id = ?1"), params![id], account_from_row)?)
}

impl Ledger {
    pub fn add_account(&mut self, name: &str, currency: &str, decimals: u32, note: Option<&str>) -> Result<Account> {
        todo!()
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_and_lists_with_uppercase_currency() {
        let mut l = Ledger::open_in_memory().unwrap();
        let a = l.add_account(" poly-usdc ", "usdc", 6, Some("polymarket proxy wallet")).unwrap();
        assert_eq!(a.name, "poly-usdc");
        assert_eq!(a.currency, "USDC");
        assert_eq!(a.decimals, 6);
        l.add_account("kalshi-usd", "USD", 2, None).unwrap();
        let names: Vec<String> = l.list_accounts().unwrap().into_iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["kalshi-usd", "poly-usdc"]);
    }

    #[test]
    fn duplicate_name_is_case_insensitive() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_account("Wallet", "USD", 2, None).unwrap();
        assert!(matches!(l.add_account("wallet", "USD", 2, None), Err(LedgerError::AccountExists(_))));
    }

    #[test]
    fn rejects_empty_name_or_currency() {
        let mut l = Ledger::open_in_memory().unwrap();
        assert!(matches!(l.add_account("  ", "USD", 2, None), Err(LedgerError::InvalidAccountName)));
        assert!(matches!(l.add_account("x", " ", 2, None), Err(LedgerError::InvalidCurrency)));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_account("Poly-USDC", "USDC", 6, None).unwrap();
        let row = account_by_name(&l.conn, "poly-usdc").unwrap();
        assert_eq!(row.name, "Poly-USDC");
        assert!(matches!(account_by_name(&l.conn, "nope"), Err(LedgerError::AccountNotFound(_))));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test accounts`
Expected: `not yet implemented`.

- [ ] **Step 4: Implement accounts**

```rust
impl Ledger {
    pub fn add_account(&mut self, name: &str, currency: &str, decimals: u32, note: Option<&str>) -> Result<Account> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LedgerError::InvalidAccountName);
        }
        let currency = currency.trim().to_uppercase();
        if currency.is_empty() {
            return Err(LedgerError::InvalidCurrency);
        }
        let tx = self.write_tx()?;
        if account_by_name(&tx, name).is_ok() {
            return Err(LedgerError::AccountExists(name.to_string()));
        }
        tx.execute(
            "INSERT INTO accounts (name, currency, decimals, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, currency, decimals as i64, note.map(str::trim), time::now()],
        )?;
        let id = tx.last_insert_rowid();
        let account = load_account(&tx, id)?;
        tx.commit()?;
        Ok(account)
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(&format!("{ACCOUNT_SELECT} ORDER BY name COLLATE NOCASE"))?;
        let rows = stmt.query_map([], account_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
```

Add to `src/lib.rs`: `pub mod ledger; pub use ledger::Ledger;`.

- [ ] **Step 5: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS (dead-code warnings for unused helpers are not allowed under `-D warnings`; add `#[allow(dead_code)]` on the `mod.rs` helpers not yet used — remove the allow in Task 10 when every helper has a caller).

```bash
git add src/lib.rs src/ledger/mod.rs src/ledger/accounts.rs
git commit -m "feat: ledger core helpers and account management"
```

---

### Task 5: add and show

**Files:**
- Create: `src/ledger/entries.rs`
- Modify: `src/ledger/mod.rs` (add `mod entries; pub use entries::{AddRequest, TransferRequest};`)

**Interfaces:**
- Consumes: everything from Task 4.
- Produces: `AddRequest { account, amount, kind, reference, memo, ts, group, meta, actor }` (all `String`/`Option<String>` except `kind: Kind`), `Ledger::add(&mut self, &AddRequest) -> Result<AddResult>`, `Ledger::show(&self, i64) -> Result<ShowResult>`.

- [ ] **Step 1: Write failing tests**

`src/ledger/entries.rs`:

```rust
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::{
    account_by_name, balance_minor, clean_ref, existing_ref, load_entry, parse_account_amount, resolve_ts,
    validate_group, validate_meta, write_entry, Ledger, NewEntry, Written,
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
        todo!()
    }

    pub fn show(&self, id: i64) -> Result<ShowResult> {
        todo!()
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
        let r = l.add(&AddRequest { reference: Some("0xabc".into()), actor: Some("claude".into()), ..req("poly-usdc", "100", Kind::Deposit) }).unwrap();
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
        let first = l.add(&AddRequest { reference: Some("tx1".into()), ..req("w", "10", Kind::Deposit) }).unwrap();
        let again = l.add(&AddRequest { reference: Some("tx1".into()), ..req("w", "10.00", Kind::Deposit) }).unwrap();
        assert!(again.duplicate);
        assert_eq!(again.entry.id, first.entry.id);
        assert_eq!(again.balance, "10.00");
        let conflict = l.add(&AddRequest { reference: Some("tx1".into()), ..req("w", "11", Kind::Deposit) });
        assert!(matches!(conflict, Err(LedgerError::RefConflict { existing_id: 1, .. })));
        assert_eq!(l.balance("w", None).unwrap().balance, "10.00");
    }

    #[test]
    fn validates_sign_precision_kind_group_meta_and_ts() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        assert!(matches!(l.add(&req("w", "-1", Kind::Deposit)), Err(LedgerError::InvalidSign { .. })));
        assert!(matches!(l.add(&req("w", "1", Kind::Fee)), Err(LedgerError::InvalidSign { .. })));
        assert!(matches!(l.add(&req("w", "0", Kind::Trade)), Err(LedgerError::ZeroAmount)));
        assert!(matches!(l.add(&req("w", "1.001", Kind::Trade)), Err(LedgerError::PrecisionExceeded { scale: 3, .. })));
        assert!(matches!(l.add(&req("w", "abc", Kind::Trade)), Err(LedgerError::InvalidAmount(_))));
        assert!(matches!(l.add(&AddRequest { group: Some("  ".into()), ..req("w", "1", Kind::Trade) }), Err(LedgerError::InvalidGroup)));
        assert!(matches!(l.add(&AddRequest { meta: Some("[1]".into()), ..req("w", "1", Kind::Trade) }), Err(LedgerError::InvalidMeta(_))));
        assert!(matches!(l.add(&AddRequest { ts: Some("later".into()), ..req("w", "1", Kind::Trade) }), Err(LedgerError::InvalidTimestamp(_))));
        assert!(matches!(l.add(&req("nope", "1", Kind::Trade)), Err(LedgerError::AccountNotFound(_))));
        assert_eq!(l.balance("w", None).unwrap().balance, "0.00");
    }

    #[test]
    fn stores_ts_group_and_meta_verbatim() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        let r = l.add(&AddRequest {
            ts: Some("2026-09-06T12:00:00+09:00".into()),
            group: Some("arb:1".into()),
            meta: Some(r#"{"market":"btc-5m","price":"0.51"}"#.into()),
            memo: Some("buy".into()),
            ..req("w", "-5", Kind::Trade)
        }).unwrap();
        assert_eq!(r.entry.ts, "2026-09-06T03:00:00.000Z");
        assert_eq!(r.entry.group_id.as_deref(), Some("arb:1"));
        assert_eq!(r.entry.meta.as_ref().unwrap()["market"], "btc-5m");
        let shown = l.show(r.entry.id).unwrap();
        assert_eq!(shown.entry.memo.as_deref(), Some("buy"));
        assert_eq!(shown.balance, "-5.00");
        assert!(matches!(l.show(99), Err(LedgerError::EntryNotFound(99))));
    }
}
```

The `balance()` call used in these tests is implemented in Task 7; until then, add this temporary shim at the bottom of `entries.rs` **and delete it in Task 7**:

```rust
#[cfg(test)]
impl Ledger {
    pub(crate) fn balance(&self, account: &str, at: Option<&str>) -> Result<crate::model::BalanceAt> {
        let acc = account_by_name(&self.conn, account)?;
        let minor = balance_minor(&self.conn, acc.id, at)?;
        Ok(crate::model::BalanceAt { account: acc.name, currency: acc.currency, balance: format_amount(minor, acc.decimals), at: at.map(str::to_string) })
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test entries`
Expected: `not yet implemented`.

- [ ] **Step 3: Implement add and show**

```rust
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
        Ok(AddResult { entry, balance, duplicate })
    }

    pub fn show(&self, id: i64) -> Result<ShowResult> {
        let entry = load_entry(&self.conn, id)?;
        let account = account_by_name(&self.conn, &entry.account)?;
        let balance = format_amount(balance_minor(&self.conn, account.id, None)?, account.decimals);
        Ok(ShowResult { entry, balance })
    }
}
```

Add to `src/ledger/mod.rs`: `mod entries;` and `pub use entries::{AddRequest, TransferRequest};`.

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add src/ledger/mod.rs src/ledger/entries.rs
git commit -m "feat: add entries with ref idempotency, show entry"
```

---

### Task 6: transfer and reverse

**Files:**
- Modify: `src/ledger/entries.rs`

**Interfaces:**
- Produces: `Ledger::transfer(&mut self, &TransferRequest) -> Result<TransferResult>`, `Ledger::reverse_entry(&mut self, id: i64, memo: Option<String>, actor: Option<String>) -> Result<ReverseResult>`, `Ledger::reverse_group(&mut self, group: &str, memo: Option<String>, actor: Option<String>) -> Result<ReverseResult>`.

- [ ] **Step 1: Write failing tests (append inside `mod tests`)**

```rust
    fn treq(from: &str, to: &str, amount: &str) -> TransferRequest {
        TransferRequest {
            from: from.into(), to: to.into(), amount: amount.into(),
            reference: None, memo: None, ts: None, group: None, meta: None, actor: None,
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

        assert!(matches!(l.transfer(&treq("a", "c", "1")), Err(LedgerError::CurrencyMismatch { .. })));
        assert!(matches!(l.transfer(&treq("a", "a", "1")), Err(LedgerError::SameAccount)));
        assert!(matches!(l.transfer(&treq("a", "b", "-1")), Err(LedgerError::InvalidSign { .. })));
        assert!(matches!(l.transfer(&treq("a", "b", "0")), Err(LedgerError::ZeroAmount)));
        assert_eq!(l.balance("a", None).unwrap().balance, "60.00");
    }

    #[test]
    fn transfer_with_ref_is_idempotent_and_conflicts_on_mismatch() {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USD", 2)]);
        let first = l.transfer(&TransferRequest { reference: Some("mv1".into()), group: Some("g1".into()), ..treq("a", "b", "5") }).unwrap();
        assert_eq!(first.entries[0].group_id.as_deref(), Some("g1"));
        let again = l.transfer(&TransferRequest { reference: Some("mv1".into()), ..treq("a", "b", "5") }).unwrap();
        assert!(again.duplicate);
        assert_eq!(again.entries[0].id, first.entries[0].id);
        let conflict = l.transfer(&TransferRequest { reference: Some("mv1".into()), ..treq("a", "b", "6") });
        assert!(matches!(conflict, Err(LedgerError::RefConflict { .. })));
        assert_eq!(l.balance("b", None).unwrap().balance, "5.00");
    }

    #[test]
    fn reverse_single_entry_inherits_group_and_links_back() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        let e = l.add(&AddRequest { group: Some("arb:1".into()), ..req("w", "-5", Kind::Trade) }).unwrap().entry;
        let r = l.reverse_entry(e.id, Some("oops".into()), Some("claude".into())).unwrap();
        assert_eq!(r.entries.len(), 1);
        let rev = &r.entries[0];
        assert_eq!(rev.kind, Kind::Reversal);
        assert_eq!(rev.amount, "5.00");
        assert_eq!(rev.reverses_id, Some(e.id));
        assert_eq!(rev.group_id.as_deref(), Some("arb:1"));
        assert_eq!(rev.memo.as_deref(), Some("oops"));
        assert_eq!(l.show(e.id).unwrap().entry.reversed_by, Some(rev.id));
        assert_eq!(l.balance("w", None).unwrap().balance, "0.00");

        assert!(matches!(l.reverse_entry(e.id, None, None), Err(LedgerError::AlreadyReversed(_, _))));
        assert!(matches!(l.reverse_entry(rev.id, None, None), Err(LedgerError::CannotReverseReversal(_))));
        assert!(matches!(l.reverse_entry(999, None, None), Err(LedgerError::EntryNotFound(999))));
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
        l.add(&AddRequest { group: Some("arb:9".into()), ..req("a", "-45", Kind::Trade) }).unwrap();
        let leg_b = l.add(&AddRequest { group: Some("arb:9".into()), ..req("b", "-52", Kind::Trade) }).unwrap().entry;
        l.reverse_entry(leg_b.id, None, None).unwrap();
        let r = l.reverse_group("arb:9", None, None).unwrap();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].account, "a");
        assert!(matches!(l.reverse_group("arb:9", None, None), Err(LedgerError::NothingToReverse(_))));
        assert!(matches!(l.reverse_group("missing", None, None), Err(LedgerError::GroupNotFound(_))));
        assert!(matches!(l.reverse_group(" ", None, None), Err(LedgerError::InvalidGroup)));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test entries`
Expected: compile errors for missing `transfer`, `reverse_entry`, `reverse_group`.

- [ ] **Step 3: Implement**

```rust
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
                from: from.name, from_currency: from.currency, to: to.name, to_currency: to.currency,
            });
        }
        let amount = parse_account_amount(&req.amount, &from)?;
        if amount == 0 {
            return Err(LedgerError::ZeroAmount);
        }
        if amount < 0 {
            return Err(LedgerError::InvalidSign {
                kind: "transfer".into(), expected: "positive", amount: format_amount(amount, from.decimals),
            });
        }
        let ts = resolve_ts(req.ts.as_deref())?;
        let group = validate_group(req.group.as_deref())?.unwrap_or_else(|| Uuid::new_v4().to_string());
        let meta = validate_meta(req.meta.as_deref())?;
        let reference = clean_ref(req.reference.as_deref());

        if let Some(r) = &reference {
            let a = existing_ref(&tx, from.id, r)?;
            let b = existing_ref(&tx, to.id, r)?;
            match (a, b) {
                (Some(x), Some(y))
                    if x.kind == "transfer" && y.kind == "transfer" && x.amount == -amount && y.amount == amount =>
                {
                    let entries = vec![load_entry(&tx, x.id)?, load_entry(&tx, y.id)?];
                    return Ok(TransferResult { entries, duplicate: true });
                }
                (None, None) => {}
                (Some(x), _) => return Err(conflict(&from, r, x)),
                (_, Some(y)) => return Err(conflict(&to, r, y)),
            }
        }

        let mut entries = Vec::with_capacity(2);
        for (account, signed) in [(&from, -amount), (&to, amount)] {
            let new = NewEntry {
                account, ts: ts.clone(), kind: Kind::Transfer, amount: signed,
                reference: reference.clone(), memo: req.memo.clone(), actor: req.actor.clone(),
                group_id: Some(group.clone()), meta: meta.clone(), reverses_id: None,
            };
            let Written::Inserted(id) = write_entry(&tx, &new)? else {
                unreachable!("refs were checked above")
            };
            entries.push(load_entry(&tx, id)?);
        }
        tx.commit()?;
        Ok(TransferResult { entries, duplicate: false })
    }

    pub fn reverse_entry(&mut self, id: i64, memo: Option<String>, actor: Option<String>) -> Result<ReverseResult> {
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

    pub fn reverse_group(&mut self, group: &str, memo: Option<String>, actor: Option<String>) -> Result<ReverseResult> {
        let group = validate_group(Some(group))?.expect("Some in, Some out");
        let tx = self.write_tx()?;
        let total: i64 = tx.query_row("SELECT count(*) FROM entries WHERE group_id = ?1", params![&group], |r| r.get(0))?;
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
        let targets = ids.into_iter().map(|id| load_entry(&tx, id)).collect::<Result<Vec<_>>>()?;
        let entries = reverse_all(&tx, &targets, memo, actor)?;
        tx.commit()?;
        Ok(ReverseResult { entries })
    }
}

fn conflict(account: &super::AccountRow, reference: &str, existing: super::ExistingRef) -> LedgerError {
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
            account: &account, ts: ts.clone(), kind: Kind::Reversal, amount: -t.amount_minor,
            reference: None, memo: memo.clone(), actor: actor.clone(), group_id: t.group_id.clone(),
            meta: None, reverses_id: Some(t.id),
        };
        let Written::Inserted(id) = write_entry(tx, &new)? else {
            unreachable!("reversals carry no ref")
        };
        out.push(load_entry(tx, id)?);
    }
    Ok(out)
}
```

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add src/ledger/entries.rs
git commit -m "feat: atomic transfers and reversal entries"
```

---

### Task 7: balances, history, export, group

**Files:**
- Create: `src/ledger/reports.rs`
- Modify: `src/ledger/mod.rs` (add `mod reports; pub use reports::HistoryFilter;`), `src/ledger/entries.rs` (delete the test-only `balance` shim)

**Interfaces:**
- Produces: `HistoryFilter { since: Option<String>, until: Option<String>, kind: Option<Kind>, group: Option<String>, limit: usize }` (derive `Default`; `limit == 0` means unlimited), `Ledger::balances(&self) -> Result<Vec<AccountBalance>>`, `Ledger::balance(&self, account: &str, at: Option<&str>) -> Result<BalanceAt>`, `Ledger::history(&self, account: &str, &HistoryFilter) -> Result<History>`, `Ledger::export(&self, account: &str) -> Result<History>`, `Ledger::group(&self, id: &str) -> Result<GroupView>`.

- [ ] **Step 1: Write failing tests**

`src/ledger/reports.rs`:

```rust
use std::collections::BTreeMap;

use rusqlite::params;

use super::{account_by_name, balance_minor, entry_from_row, normalize_opt_ts, validate_group, Ledger, ENTRY_SELECT};
use crate::error::{LedgerError, Result};
use crate::model::{AccountBalance, BalanceAt, Entry, GroupView, History, HistoryEntry, Kind};
use crate::money::{format_amount, format_wide};

#[derive(Clone, Debug, Default)]
pub struct HistoryFilter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub kind: Option<Kind>,
    pub group: Option<String>,
    /// 0 means unlimited.
    pub limit: usize,
}

impl Ledger {
    pub fn balances(&self) -> Result<Vec<AccountBalance>> {
        todo!()
    }

    pub fn balance(&self, account: &str, at: Option<&str>) -> Result<BalanceAt> {
        todo!()
    }

    pub fn history(&self, account: &str, filter: &HistoryFilter) -> Result<History> {
        todo!()
    }

    pub fn export(&self, account: &str) -> Result<History> {
        self.history(account, &HistoryFilter::default())
    }

    pub fn group(&self, id: &str) -> Result<GroupView> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::entries::tests::{ledger_with, req};
    use crate::ledger::AddRequest;

    fn at(ts: &str, r: AddRequest) -> AddRequest {
        AddRequest { ts: Some(ts.into()), ..r }
    }

    #[test]
    fn balances_lists_every_account_with_counts() {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USDC", 6)]);
        l.add(&at("2026-09-01", req("a", "10", Kind::Deposit))).unwrap();
        l.add(&at("2026-09-02", req("a", "-4", Kind::Trade))).unwrap();
        let all = l.balances().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].account, "a");
        assert_eq!(all[0].balance, "6.00");
        assert_eq!(all[0].entries, 2);
        assert_eq!(all[0].last_ts.as_deref(), Some("2026-09-02T00:00:00.000Z"));
        assert_eq!(all[0].last_reconciled_at, None);
        assert_eq!(all[1].balance, "0.000000");
        assert_eq!(all[1].entries, 0);
    }

    #[test]
    fn balance_at_uses_movement_time() {
        let mut l = ledger_with(&[("a", "USD", 2)]);
        l.add(&at("2026-09-01T00:00:00Z", req("a", "10", Kind::Deposit))).unwrap();
        l.add(&at("2026-09-03T00:00:00Z", req("a", "-4", Kind::Trade))).unwrap();
        let b = l.balance("a", Some("2026-09-02")).unwrap();
        assert_eq!(b.balance, "10.00");
        assert_eq!(b.at.as_deref(), Some("2026-09-02T00:00:00.000Z"));
        assert_eq!(l.balance("a", None).unwrap().balance, "6.00");
        assert!(matches!(l.balance("a", Some("bad")), Err(LedgerError::InvalidTimestamp(_))));
    }

    #[test]
    fn history_has_true_running_balance_under_filters() {
        let mut l = ledger_with(&[("a", "USD", 2)]);
        l.add(&at("2026-09-01", req("a", "10", Kind::Deposit))).unwrap();
        l.add(&at("2026-09-02", AddRequest { group: Some("g".into()), ..req("a", "-4", Kind::Trade) })).unwrap();
        l.add(&at("2026-09-03", req("a", "-1", Kind::Fee))).unwrap();
        l.add(&at("2026-09-04", req("a", "3", Kind::Settlement))).unwrap();

        let full = l.history("a", &HistoryFilter::default()).unwrap();
        let after: Vec<&str> = full.entries.iter().map(|e| e.balance_after.as_str()).collect();
        assert_eq!(after, ["10.00", "6.00", "5.00", "8.00"]);

        let last_two = l.history("a", &HistoryFilter { limit: 2, ..Default::default() }).unwrap();
        assert_eq!(last_two.entries.len(), 2);
        assert_eq!(last_two.entries[0].balance_after, "5.00");
        assert_eq!(last_two.entries[1].balance_after, "8.00");

        let since = l.history("a", &HistoryFilter { since: Some("2026-09-03".into()), ..Default::default() }).unwrap();
        assert_eq!(since.entries.len(), 2);
        assert_eq!(since.entries[0].balance_after, "5.00");

        let fees = l.history("a", &HistoryFilter { kind: Some(Kind::Fee), ..Default::default() }).unwrap();
        assert_eq!(fees.entries.len(), 1);
        assert_eq!(fees.entries[0].balance_after, "5.00");

        let grouped = l.history("a", &HistoryFilter { group: Some("g".into()), ..Default::default() }).unwrap();
        assert_eq!(grouped.entries.len(), 1);
        assert_eq!(grouped.entries[0].entry.amount, "-4.00");

        let until = l.history("a", &HistoryFilter { until: Some("2026-09-02".into()), ..Default::default() }).unwrap();
        assert_eq!(until.entries.len(), 2);
    }

    #[test]
    fn group_view_spans_accounts_and_nets_per_currency() {
        let mut l = ledger_with(&[("poly", "USDC", 6), ("kalshi", "USD", 2), ("poly2", "USDC", 2)]);
        let g = Some("arb:1".to_string());
        l.add(&AddRequest { group: g.clone(), ..req("poly", "-45", Kind::Trade) }).unwrap();
        l.add(&AddRequest { group: g.clone(), ..req("kalshi", "-52", Kind::Trade) }).unwrap();
        l.add(&AddRequest { group: g.clone(), ..req("poly", "100", Kind::Settlement) }).unwrap();
        l.add(&AddRequest { group: g.clone(), ..req("poly2", "0.5", Kind::Other) }).unwrap();
        let v = l.group("arb:1").unwrap();
        assert_eq!(v.entries.len(), 4);
        assert_eq!(v.net["USDC"], "55.500000");
        assert_eq!(v.net["USD"], "-52.00");
        assert!(matches!(l.group("nope"), Err(LedgerError::GroupNotFound(_))));

        l.reverse_group("arb:1", None, None).unwrap();
        let after = l.group("arb:1").unwrap();
        assert_eq!(after.entries.len(), 8);
        assert_eq!(after.net["USDC"], "0.000000");
        assert_eq!(after.net["USD"], "0.00");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test reports`
Expected: `not yet implemented` (and remove the shim from `entries.rs` so there is a single `balance`).

- [ ] **Step 3: Implement**

```rust
impl Ledger {
    pub fn balances(&self) -> Result<Vec<AccountBalance>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.name, a.currency, a.decimals, COALESCE(SUM(e.amount), 0), COUNT(e.id), MAX(e.ts), \
                    (SELECT MAX(s.ts) FROM snapshots s WHERE s.account_id = a.id) \
             FROM accounts a LEFT JOIN entries e ON e.account_id = a.id \
             GROUP BY a.id ORDER BY a.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            let decimals = r.get::<_, i64>(2)? as u32;
            Ok(AccountBalance {
                account: r.get(0)?,
                currency: r.get(1)?,
                balance: format_amount(r.get(3)?, decimals),
                entries: r.get(4)?,
                last_ts: r.get(5)?,
                last_reconciled_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn balance(&self, account: &str, at: Option<&str>) -> Result<BalanceAt> {
        let acc = account_by_name(&self.conn, account)?;
        let at = normalize_opt_ts(at)?;
        let minor = balance_minor(&self.conn, acc.id, at.as_deref())?;
        Ok(BalanceAt { account: acc.name, currency: acc.currency, balance: format_amount(minor, acc.decimals), at })
    }

    pub fn history(&self, account: &str, filter: &HistoryFilter) -> Result<History> {
        let acc = account_by_name(&self.conn, account)?;
        let since = normalize_opt_ts(filter.since.as_deref())?;
        let until = normalize_opt_ts(filter.until.as_deref())?;
        let group = validate_group(filter.group.as_deref())?;
        let limit: i64 = if filter.limit == 0 { -1 } else { filter.limit as i64 };
        let sql = "WITH running AS ( \
                     SELECT e.*, SUM(e.amount) OVER (ORDER BY e.ts, e.id) AS balance_after \
                     FROM entries e WHERE e.account_id = ?1) \
                   SELECT e.id, a.name, a.currency, a.decimals, e.ts, e.recorded_at, e.kind, e.amount, \
                          e.ref, e.memo, e.actor, e.group_id, e.meta, e.reverses_id, \
                          (SELECT r.id FROM entries r WHERE r.reverses_id = e.id), e.balance_after \
                   FROM running e JOIN accounts a ON a.id = e.account_id \
                   WHERE (?2 IS NULL OR e.ts >= ?2) AND (?3 IS NULL OR e.ts <= ?3) \
                     AND (?4 IS NULL OR e.kind = ?4) AND (?5 IS NULL OR e.group_id = ?5) \
                   ORDER BY e.ts DESC, e.id DESC LIMIT ?6";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(
            params![acc.id, since, until, filter.kind.map(Kind::as_str), group, limit],
            |r| {
                let entry = entry_from_row(r)?;
                let balance_after = format_amount(r.get::<_, i64>(15)?, entry.decimals);
                Ok(HistoryEntry { entry, balance_after })
            },
        )?;
        let mut entries = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        entries.reverse();
        Ok(History { account: acc.name, currency: acc.currency, entries })
    }

    pub fn group(&self, id: &str) -> Result<GroupView> {
        let group = validate_group(Some(id))?.expect("Some in, Some out");
        let mut stmt = self.conn.prepare(&format!("{ENTRY_SELECT} WHERE e.group_id = ?1 ORDER BY e.ts, e.id"))?;
        let entries = stmt
            .query_map(params![&group], entry_from_row)?
            .collect::<rusqlite::Result<Vec<Entry>>>()?;
        if entries.is_empty() {
            return Err(LedgerError::GroupNotFound(group));
        }
        let mut max_decimals: BTreeMap<String, u32> = BTreeMap::new();
        for e in &entries {
            let d = max_decimals.entry(e.currency.clone()).or_insert(0);
            *d = (*d).max(e.decimals);
        }
        let mut sums: BTreeMap<String, i128> = BTreeMap::new();
        for e in &entries {
            let target = max_decimals[&e.currency];
            let scaled = e.amount_minor as i128 * 10i128.pow(target - e.decimals);
            *sums.entry(e.currency.clone()).or_insert(0) += scaled;
        }
        let net = sums.into_iter().map(|(ccy, sum)| {
            let d = max_decimals[&ccy];
            (ccy, format_wide(sum, d))
        }).collect();
        Ok(GroupView { group, entries, net })
    }
}
```

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add src/ledger/mod.rs src/ledger/reports.rs src/ledger/entries.rs
git commit -m "feat: balances, history with running balance, group view"
```

---

### Task 8: realized PnL

**Files:**
- Create: `src/ledger/pnl.rs`
- Modify: `src/ledger/mod.rs` (add `mod pnl; pub use pnl::{PnlBucket, PnlFilter};`)

**Interfaces:**
- Produces: `PnlBucket::{Total, Day, Week, Month, Group, Meta(String)}` with `PnlBucket::parse(&str) -> Result<PnlBucket>`, `PnlFilter { since, until, by: PnlBucket }`, `Ledger::pnl(&self, account: Option<&str>, &PnlFilter) -> Result<Vec<AccountPnl>>`.

- [ ] **Step 1: Write failing tests**

`src/ledger/pnl.rs`:

```rust
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
        todo!()
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
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::entries::tests::{ledger_with, req};
    use crate::ledger::{AddRequest, TransferRequest};
    use crate::model::Kind;

    fn f(by: PnlBucket) -> PnlFilter {
        PnlFilter { since: None, until: None, by }
    }

    fn seeded() -> Ledger {
        let mut l = ledger_with(&[("a", "USD", 2), ("b", "USD", 2)]);
        let day = |d: &str, r: AddRequest| AddRequest { ts: Some(format!("2026-09-0{d}")), ..r };
        l.add(&day("1", req("a", "100", Kind::Deposit))).unwrap();
        l.add(&day("1", AddRequest { group: Some("g1".into()), meta: Some(r#"{"strategy":"arb"}"#.into()), ..req("a", "-40", Kind::Trade) })).unwrap();
        l.add(&day("2", AddRequest { group: Some("g1".into()), meta: Some(r#"{"strategy":"arb"}"#.into()), ..req("a", "48", Kind::Settlement) })).unwrap();
        l.add(&day("2", AddRequest { meta: Some(r#"{"strategy":"mom"}"#.into()), ..req("a", "-2", Kind::Fee) })).unwrap();
        let bad = l.add(&day("3", req("a", "-7", Kind::Trade))).unwrap().entry;
        l.reverse_entry(bad.id, None, None).unwrap();
        l.transfer(&TransferRequest { from: "a".into(), to: "b".into(), amount: "10".into(), reference: None, memo: None, ts: Some("2026-09-03".into()), group: None, meta: None, actor: None }).unwrap();
        l.add(&day("4", req("a", "-30", Kind::Withdrawal))).unwrap();
        l
    }

    #[test]
    fn parses_buckets() {
        assert_eq!(PnlBucket::parse("total").unwrap(), PnlBucket::Total);
        assert_eq!(PnlBucket::parse("day").unwrap(), PnlBucket::Day);
        assert_eq!(PnlBucket::parse("meta:strategy").unwrap(), PnlBucket::Meta("strategy".into()));
        assert!(matches!(PnlBucket::parse("meta:"), Err(LedgerError::InvalidBucket(_))));
        assert!(matches!(PnlBucket::parse("meta:a.b"), Err(LedgerError::InvalidBucket(_))));
        assert!(matches!(PnlBucket::parse("hour"), Err(LedgerError::InvalidBucket(_))));
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
        let days: Vec<(Option<String>, String)> = by_day[0].rows.iter().map(|r| (r.bucket.clone(), r.net.clone())).collect();
        assert_eq!(days, vec![
            (Some("2026-09-01".into()), "-40.00".into()),
            (Some("2026-09-02".into()), "46.00".into()),
        ]);
        let by_group = l.pnl(Some("a"), &f(PnlBucket::Group)).unwrap();
        let g1 = by_group[0].rows.iter().find(|r| r.bucket.as_deref() == Some("g1")).unwrap();
        assert_eq!(g1.net, "8.00");
        let ungrouped = by_group[0].rows.iter().find(|r| r.bucket.is_none()).unwrap();
        assert_eq!(ungrouped.net, "-2.00");
        let by_meta = l.pnl(Some("a"), &f(PnlBucket::Meta("strategy".into()))).unwrap();
        let arb = by_meta[0].rows.iter().find(|r| r.bucket.as_deref() == Some("arb")).unwrap();
        assert_eq!(arb.net, "8.00");
        let mom = by_meta[0].rows.iter().find(|r| r.bucket.as_deref() == Some("mom")).unwrap();
        assert_eq!(mom.fees, "-2.00");
    }

    #[test]
    fn since_until_filter_on_ts() {
        let l = seeded();
        let out = l.pnl(Some("a"), &PnlFilter { since: Some("2026-09-02".into()), until: None, by: PnlBucket::Total }).unwrap();
        assert_eq!(out[0].rows[0].net, "46.00");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test pnl`
Expected: `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
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
                Some(key) if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
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
                    trades: zero.clone(), settlements: zero.clone(), fees: zero.clone(),
                    adjustments: zero.clone(), other: zero.clone(), net: zero,
                });
            }
            out.push(AccountPnl { account: acc.name, currency: acc.currency, rows });
        }
        Ok(out)
    }
}
```

Note: `CAST(json_extract(...) AS TEXT)` on a JSON string value yields the bare string (`arb`), which is what the tests expect; `r.get(0)` into `Option<String>` handles the `NULL` bucket.

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add src/ledger/mod.rs src/ledger/pnl.rs
git commit -m "feat: realized pnl report with day/week/month/group/meta buckets"
```

---

### Task 9: reconcile and snapshots

**Files:**
- Create: `src/ledger/reconcile.rs`
- Modify: `src/ledger/mod.rs` (add `mod reconcile; pub use reconcile::ReconcileRequest;`)

**Interfaces:**
- Produces: `ReconcileRequest { account: String, observed: String, source: Option<String>, ts: Option<String>, adjust: bool, actor: Option<String> }`, `Ledger::reconcile(&mut self, &ReconcileRequest) -> Result<ReconcileResult>`, `Ledger::snapshots(&self, account: &str, limit: usize) -> Result<SnapshotList>` (limit 0 = unlimited).

- [ ] **Step 1: Write failing tests**

`src/ledger/reconcile.rs`:

```rust
use rusqlite::{params, Row};

use super::{account_by_name, balance_minor, load_entry, parse_account_amount, resolve_ts, write_entry, Ledger, NewEntry, Written};
use crate::error::Result;
use crate::model::{Kind, ReconcileResult, Snapshot, SnapshotList};
use crate::money::format_amount;

#[derive(Clone, Debug)]
pub struct ReconcileRequest {
    pub account: String,
    pub observed: String,
    pub source: Option<String>,
    pub ts: Option<String>,
    pub adjust: bool,
    pub actor: Option<String>,
}

const SNAPSHOT_SELECT: &str = "SELECT s.id, a.name, a.decimals, s.ts, s.observed, s.book, s.diff, s.adjustment_entry_id, s.source \
     FROM snapshots s JOIN accounts a ON a.id = s.account_id";

fn snapshot_from_row(r: &Row<'_>) -> rusqlite::Result<Snapshot> {
    let d = r.get::<_, i64>(2)? as u32;
    Ok(Snapshot {
        id: r.get(0)?,
        account: r.get(1)?,
        ts: r.get(3)?,
        observed: format_amount(r.get(4)?, d),
        book: format_amount(r.get(5)?, d),
        diff: format_amount(r.get(6)?, d),
        adjustment_entry_id: r.get(7)?,
        source: r.get(8)?,
    })
}

impl Ledger {
    pub fn reconcile(&mut self, req: &ReconcileRequest) -> Result<ReconcileResult> {
        todo!()
    }

    pub fn snapshots(&self, account: &str, limit: usize) -> Result<SnapshotList> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::entries::tests::{ledger_with, req};
    use crate::ledger::AddRequest;

    fn rreq(observed: &str) -> ReconcileRequest {
        ReconcileRequest { account: "w".into(), observed: observed.into(), source: Some("chain".into()), ts: None, adjust: true, actor: Some("claude".into()) }
    }

    #[test]
    fn posts_adjustment_so_book_matches_observed() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&req("w", "126.2", Kind::Deposit)).unwrap();
        let r = l.reconcile(&rreq("124.70")).unwrap();
        assert_eq!(r.snapshot.observed, "124.700000");
        assert_eq!(r.snapshot.book, "126.200000");
        assert_eq!(r.snapshot.diff, "-1.500000");
        assert_eq!(r.snapshot.source.as_deref(), Some("chain"));
        let adj = r.adjustment.unwrap();
        assert_eq!(adj.kind, Kind::Adjustment);
        assert_eq!(adj.amount, "-1.500000");
        assert_eq!(adj.ts, r.snapshot.ts);
        assert_eq!(adj.memo.as_deref(), Some("reconcile: observed 124.700000, book 126.200000"));
        assert_eq!(r.snapshot.adjustment_entry_id, Some(adj.id));
        assert_eq!(l.balance("w", None).unwrap().balance, "124.700000");
        assert_eq!(l.balances().unwrap()[0].last_reconciled_at.as_deref(), Some(r.snapshot.ts.as_str()));
    }

    #[test]
    fn zero_diff_and_no_adjust_write_no_entry() {
        let mut l = ledger_with(&[("w", "USDC", 6)]);
        l.add(&req("w", "10", Kind::Deposit)).unwrap();
        let same = l.reconcile(&rreq("10")).unwrap();
        assert!(same.adjustment.is_none());
        assert_eq!(same.snapshot.diff, "0.000000");
        let skipped = l.reconcile(&ReconcileRequest { adjust: false, ..rreq("12") }).unwrap();
        assert!(skipped.adjustment.is_none());
        assert_eq!(skipped.snapshot.diff, "2.000000");
        assert_eq!(l.balance("w", None).unwrap().balance, "10.000000");
        assert_eq!(l.snapshots("w", 0).unwrap().snapshots.len(), 2);
    }

    #[test]
    fn ts_computes_book_as_of_that_time() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        l.add(&AddRequest { ts: Some("2026-09-01".into()), ..req("w", "10", Kind::Deposit) }).unwrap();
        l.add(&AddRequest { ts: Some("2026-09-05".into()), ..req("w", "-3", Kind::Trade) }).unwrap();
        let r = l.reconcile(&ReconcileRequest { ts: Some("2026-09-02".into()), ..rreq("9") }).unwrap();
        assert_eq!(r.snapshot.book, "10.00");
        assert_eq!(r.snapshot.diff, "-1.00");
        assert_eq!(r.snapshot.ts, "2026-09-02T00:00:00.000Z");
        assert_eq!(l.balance("w", Some("2026-09-02")).unwrap().balance, "9.00");
        assert_eq!(l.balance("w", None).unwrap().balance, "6.00");
    }

    #[test]
    fn observed_zero_is_allowed_and_snapshots_list_oldest_first_with_limit() {
        let mut l = ledger_with(&[("w", "USD", 2)]);
        l.add(&req("w", "5", Kind::Deposit)).unwrap();
        l.reconcile(&ReconcileRequest { ts: Some("2026-09-01".into()), ..rreq("0") }).unwrap();
        l.reconcile(&ReconcileRequest { ts: Some("2026-09-02".into()), ..rreq("0") }).unwrap();
        l.reconcile(&ReconcileRequest { ts: Some("2026-09-03".into()), ..rreq("0") }).unwrap();
        let s = l.snapshots("w", 2).unwrap();
        assert_eq!(s.snapshots.len(), 2);
        assert_eq!(s.snapshots[0].ts, "2026-09-02T00:00:00.000Z");
        assert_eq!(s.snapshots[1].ts, "2026-09-03T00:00:00.000Z");
        assert_eq!(l.balance("w", None).unwrap().balance, "0.00");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test reconcile`
Expected: `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
impl Ledger {
    pub fn reconcile(&mut self, req: &ReconcileRequest) -> Result<ReconcileResult> {
        let tx = self.write_tx()?;
        let acc = account_by_name(&tx, &req.account)?;
        let observed = parse_account_amount(&req.observed, &acc)?;
        let ts = resolve_ts(req.ts.as_deref())?;
        let book = balance_minor(&tx, acc.id, Some(&ts))?;
        let diff = observed - book;

        let adjustment = if diff != 0 && req.adjust {
            let new = NewEntry {
                account: &acc,
                ts: ts.clone(),
                kind: Kind::Adjustment,
                amount: diff,
                reference: None,
                memo: Some(format!(
                    "reconcile: observed {}, book {}",
                    format_amount(observed, acc.decimals),
                    format_amount(book, acc.decimals)
                )),
                actor: req.actor.clone(),
                group_id: None,
                meta: None,
                reverses_id: None,
            };
            let Written::Inserted(id) = write_entry(&tx, &new)? else {
                unreachable!("adjustments carry no ref")
            };
            Some(load_entry(&tx, id)?)
        } else {
            None
        };

        tx.execute(
            "INSERT INTO snapshots (account_id, ts, observed, book, diff, adjustment_entry_id, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![acc.id, ts, observed, book, diff, adjustment.as_ref().map(|e| e.id), req.source.as_deref().map(str::trim)],
        )?;
        let snapshot_id = tx.last_insert_rowid();
        let snapshot = tx.query_row(&format!("{SNAPSHOT_SELECT} WHERE s.id = ?1"), params![snapshot_id], snapshot_from_row)?;
        tx.commit()?;
        Ok(ReconcileResult { snapshot, adjustment })
    }

    pub fn snapshots(&self, account: &str, limit: usize) -> Result<SnapshotList> {
        let acc = account_by_name(&self.conn, account)?;
        let limit: i64 = if limit == 0 { -1 } else { limit as i64 };
        let mut stmt = self.conn.prepare(&format!(
            "{SNAPSHOT_SELECT} WHERE s.account_id = ?1 ORDER BY s.ts DESC, s.id DESC LIMIT ?2"
        ))?;
        let mut snapshots = stmt
            .query_map(params![acc.id, limit], snapshot_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        snapshots.reverse();
        Ok(SnapshotList { account: acc.name, snapshots })
    }
}
```

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add src/ledger/mod.rs src/ledger/reconcile.rs
git commit -m "feat: reconcile with as-of book balance and snapshot listing"
```

---

### Task 10: import from JSON Lines

**Files:**
- Create: `src/ledger/import.rs`
- Modify: `src/ledger/mod.rs` (add `mod import;`; remove any remaining `#[allow(dead_code)]`)

**Interfaces:**
- Produces: `Ledger::import<R: BufRead>(&mut self, reader: R, dry_run: bool, default_actor: Option<&str>) -> Result<ImportResult>`.

- [ ] **Step 1: Write failing tests**

`src/ledger/import.rs`:

```rust
use std::io::BufRead;

use serde::Deserialize;

use super::{account_by_name, clean_ref, load_entry, meta_value_to_storage, parse_account_amount, resolve_ts, validate_group, write_entry, Ledger, NewEntry, Written};
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
    LedgerError::Import { line, source: Box::new(e) }
}

impl Ledger {
    pub fn import<R: BufRead>(&mut self, reader: R, dry_run: bool, default_actor: Option<&str>) -> Result<ImportResult> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(l.history("poly", &HistoryFilter::default()).unwrap().entries.len(), 2);
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
        assert_eq!(l.import(transfer.as_bytes(), false, None).unwrap_err().code(), "invalid_kind");

        let bad_meta = "{\"account\":\"poly\",\"amount\":\"1\",\"kind\":\"deposit\",\"meta\":[1]}\n";
        assert_eq!(l.import(bad_meta.as_bytes(), false, None).unwrap_err().code(), "invalid_meta");
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test import`
Expected: `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
impl Ledger {
    pub fn import<R: BufRead>(&mut self, reader: R, dry_run: bool, default_actor: Option<&str>) -> Result<ImportResult> {
        let mut lines: Vec<(usize, ImportLine)> = Vec::new();
        for (idx, raw) in reader.lines().enumerate() {
            let n = idx + 1;
            let raw = raw?;
            if raw.trim().is_empty() {
                continue;
            }
            let parsed: ImportLine =
                serde_json::from_str(&raw).map_err(|e| at_line(n, LedgerError::InvalidJson(e.to_string())))?;
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
                    other => return Err(LedgerError::InvalidAmount(format!("{other} (amount must be a JSON string)"))),
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
                    actor: line.actor.clone().or_else(|| default_actor.map(str::to_string)),
                    group_id: validate_group(line.group.as_deref())?,
                    meta,
                    reverses_id: None,
                };
                write_entry(&tx, &new)
            })()
            .map_err(|e| at_line(*n, e))?;
            let id = match written {
                Written::Inserted(id) => { imported += 1; id }
                Written::Duplicate(id) => { duplicates += 1; id }
            };
            entries.push(load_entry(&tx, id)?);
        }
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()?;
        }
        Ok(ImportResult { imported, duplicates, dry_run, entries })
    }
}
```

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS. No `#[allow(dead_code)]` remains in `src/ledger/mod.rs`.

```bash
git add src/ledger/mod.rs src/ledger/import.rs
git commit -m "feat: all-or-nothing JSON Lines import with dry-run"
```

---

### Task 11: CLI arguments, dispatch, JSON output, exit codes

**Files:**
- Create: `src/cli/mod.rs`, `src/cli/output.rs`, `src/cli/render.rs` (JSON-free stub returning `String::new()` for now), `tests/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: every public `Ledger` method, `Kind::parse`, `Kind::parse_addable`, `PnlBucket::parse`, `LedgerError::{code, exit_code, line}`.
- Produces: `cli::Cli` (clap), `cli::output::Output` (serde untagged), `cli::render::{render(&Output) -> String, csv(&History) -> String}`.

- [ ] **Step 1: Write `src/cli/mod.rs`**

```rust
pub mod output;
pub mod render;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "ledger", version, about = "Append-only SQLite ledger for AI agents")]
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

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ExportFormat {
    Csv,
    Json,
}
```

- [ ] **Step 2: Write `src/cli/output.rs`**

```rust
use serde::Serialize;

use agent_ledger::model::*;

/// One value per command. Untagged so struct variants serialize as their field map.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Output {
    Account { account: Account },
    Accounts { accounts: Vec<Account> },
    Add(AddResult),
    Show(ShowResult),
    Transfer(TransferResult),
    Reverse(ReverseResult),
    Balances { accounts: Vec<AccountBalance> },
    Balance(BalanceAt),
    History(History),
    Group(GroupView),
    Pnl { accounts: Vec<AccountPnl> },
    Reconcile(ReconcileResult),
    Snapshots(SnapshotList),
    Import(ImportResult),
    /// Pre-rendered text (CSV) printed verbatim regardless of --json.
    #[serde(skip)]
    Raw(String),
}
```

- [ ] **Step 3: Stub `src/cli/render.rs`**

```rust
use agent_ledger::model::History;

use super::output::Output;

pub fn render(_out: &Output) -> String {
    String::new()
}

pub fn csv(_history: &History) -> String {
    String::new()
}
```

- [ ] **Step 4: Write `src/main.rs`**

```rust
mod cli;

use std::path::PathBuf;

use clap::Parser;

use agent_ledger::ledger::{AddRequest, HistoryFilter, PnlBucket, PnlFilter, ReconcileRequest, TransferRequest};
use agent_ledger::model::Kind;
use agent_ledger::{Ledger, LedgerError};

use cli::output::Output;
use cli::{AccountCommand, Cli, Command, ExportFormat};

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() { 1 } else { 0 };
            let _ = e.print();
            std::process::exit(code);
        }
    };
    let json = cli.json;
    match run(cli) {
        Ok(Output::Raw(text)) => print!("{text}"),
        Ok(out) if json => println!("{}", serde_json::to_string_pretty(&out).expect("output is serializable")),
        Ok(out) => print!("{}", cli::render::render(&out)),
        Err(e) => {
            if json {
                let mut obj = serde_json::json!({ "code": e.code(), "message": e.to_string() });
                if let Some(line) = e.line() {
                    obj["line"] = serde_json::json!(line);
                }
                eprintln!("{}", serde_json::json!({ "error": obj }));
            } else {
                eprintln!("error: {e}");
            }
            std::process::exit(e.exit_code());
        }
    }
}

fn db_path(cli: &Cli) -> PathBuf {
    cli.db.clone().unwrap_or_else(|| {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".agent-ledger").join("ledger.db")
    })
}

fn run(cli: Cli) -> Result<Output, LedgerError> {
    let path = db_path(&cli);
    let actor = cli.actor.clone();
    let mut ledger = Ledger::open(&path)?;
    Ok(match cli.command {
        Command::Account { command: AccountCommand::Add { name, currency, decimals, note } } => {
            Output::Account { account: ledger.add_account(&name, &currency, decimals, note.as_deref())? }
        }
        Command::Account { command: AccountCommand::List } => Output::Accounts { accounts: ledger.list_accounts()? },
        Command::Add(a) => Output::Add(ledger.add(&AddRequest {
            account: a.account,
            amount: a.amount,
            kind: Kind::parse_addable(&a.kind)?,
            reference: a.reference,
            memo: a.memo,
            ts: a.ts,
            group: a.group,
            meta: a.meta,
            actor,
        })?),
        Command::Transfer(t) => Output::Transfer(ledger.transfer(&TransferRequest {
            from: t.from,
            to: t.to,
            amount: t.amount,
            reference: t.reference,
            memo: t.memo,
            ts: t.ts,
            group: t.group,
            meta: t.meta,
            actor,
        })?),
        Command::Reverse(r) => Output::Reverse(match (r.entry_id, r.group) {
            (Some(id), _) => ledger.reverse_entry(id, r.memo, actor)?,
            (None, Some(group)) => ledger.reverse_group(&group, r.memo, actor)?,
            (None, None) => unreachable!("clap requires entry_id or --group"),
        }),
        Command::Balance(b) => match b.account {
            Some(account) => Output::Balance(ledger.balance(&account, b.at.as_deref())?),
            None => Output::Balances { accounts: ledger.balances()? },
        },
        Command::History(h) => {
            let kind = match h.kind {
                None => None,
                Some(k) => Some(Kind::parse(&k).ok_or(LedgerError::InvalidKind(k))?),
            };
            Output::History(ledger.history(
                &h.account,
                &HistoryFilter { since: h.since, until: h.until, kind, group: h.group, limit: h.limit },
            )?)
        }
        Command::Group { id } => Output::Group(ledger.group(&id)?),
        Command::Pnl(p) => Output::Pnl {
            accounts: ledger.pnl(
                p.account.as_deref(),
                &PnlFilter { since: p.since, until: p.until, by: PnlBucket::parse(&p.by)? },
            )?,
        },
        Command::Reconcile(r) => Output::Reconcile(ledger.reconcile(&ReconcileRequest {
            account: r.account,
            observed: r.observed,
            source: r.source,
            ts: r.ts,
            adjust: !r.no_adjust,
            actor,
        })?),
        Command::Snapshots(s) => Output::Snapshots(ledger.snapshots(&s.account, s.limit)?),
        Command::Import(i) => {
            let stdin = std::io::stdin();
            Output::Import(ledger.import(stdin.lock(), i.dry_run, actor.as_deref())?)
        }
        Command::Show { entry_id } => Output::Show(ledger.show(entry_id)?),
        Command::Export(e) => {
            let history = ledger.export(&e.account)?;
            match e.format {
                ExportFormat::Json => Output::History(history),
                ExportFormat::Csv => Output::Raw(cli::render::csv(&history)),
            }
        }
    })
}
```

- [ ] **Step 5: Write failing end-to-end tests**

`tests/cli.rs`:

```rust
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;

fn ledger(db: &Path) -> Command {
    let mut c = Command::cargo_bin("ledger").unwrap();
    c.arg("--db").arg(db).env_remove("LEDGER_ACTOR").env_remove("LEDGER_DB");
    c
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| panic!("not json: {e}\n{}", String::from_utf8_lossy(bytes)))
}

fn with_account(db: &Path) {
    ledger(db).args(["account", "add", "poly-usdc", "--currency", "usdc", "--decimals", "6"]).assert().success();
}

#[test]
fn add_balance_history_json_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);

    let out = ledger(&db)
        .args(["--json", "--actor", "claude", "add", "poly-usdc", "100", "--kind", "deposit", "--ref", "0xabc"])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out.stdout);
    assert_eq!(v["entry"]["amount"], "100.000000");
    assert_eq!(v["entry"]["currency"], "USDC");
    assert_eq!(v["entry"]["ref"], "0xabc");
    assert_eq!(v["entry"]["actor"], "claude");
    assert_eq!(v["balance"], "100.000000");
    assert_eq!(v["duplicate"], false);

    ledger(&db).args(["add", "poly-usdc", "-25.5", "--kind", "trade", "--group", "g1", "--meta", r#"{"side":"buy"}"#]).assert().success();

    let v = json(&ledger(&db).args(["--json", "balance"]).output().unwrap().stdout);
    assert_eq!(v["accounts"][0]["account"], "poly-usdc");
    assert_eq!(v["accounts"][0]["balance"], "74.500000");
    assert_eq!(v["accounts"][0]["entries"], 2);

    let v = json(&ledger(&db).args(["--json", "balance", "poly-usdc"]).output().unwrap().stdout);
    assert_eq!(v["balance"], "74.500000");
    assert!(v["at"].is_null());

    let v = json(&ledger(&db).args(["--json", "history", "poly-usdc", "--limit", "1"]).output().unwrap().stdout);
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    assert_eq!(v["entries"][0]["balance_after"], "74.500000");
    assert_eq!(v["entries"][0]["meta"]["side"], "buy");
    assert_eq!(v["entries"][0]["group_id"], "g1");
}

#[test]
fn duplicate_ref_exits_0_and_conflict_exits_2_with_json_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    let args = ["--json", "add", "poly-usdc", "10", "--kind", "deposit", "--ref", "tx1"];
    ledger(&db).args(args).assert().success();
    let v = json(&ledger(&db).args(args).output().unwrap().stdout);
    assert_eq!(v["duplicate"], true);

    let out = ledger(&db).args(["--json", "add", "poly-usdc", "11", "--kind", "deposit", "--ref", "tx1"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let e = json(&out.stderr);
    assert_eq!(e["error"]["code"], "ref_conflict");
    assert!(e["error"]["message"].as_str().unwrap().contains("tx1"));
}

#[test]
fn exit_codes_for_domain_and_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    ledger(&db).args(["add", "nope", "1", "--kind", "deposit"]).assert().code(2).stderr(predicates::str::contains("not found"));
    ledger(&db).args(["add", "poly-usdc", "5", "--kind", "fee"]).assert().code(2).stderr(predicates::str::contains("negative"));
    ledger(&db).args(["add", "poly-usdc", "1", "--kind", "transfer"]).assert().code(2);
    ledger(&db).args(["frobnicate"]).assert().code(1);
    ledger(&db).args(["reverse"]).assert().code(1);
    ledger(&db).args(["account", "add", "x", "--currency", "USD", "--decimals", "19"]).assert().code(1);
    ledger(&db).args(["--help"]).assert().code(0);
}

#[test]
fn import_from_stdin_with_dry_run_then_real() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    let lines = "{\"account\":\"poly-usdc\",\"amount\":\"100\",\"kind\":\"deposit\",\"ref\":\"d1\"}\n\
                 {\"account\":\"poly-usdc\",\"amount\":\"-40\",\"kind\":\"trade\",\"ref\":\"t1\",\"group\":\"g\"}\n";
    let v = json(&ledger(&db).args(["--json", "import", "--dry-run"]).write_stdin(lines).output().unwrap().stdout);
    assert_eq!(v["imported"], 2);
    assert_eq!(v["dry_run"], true);
    let v = json(&ledger(&db).args(["--json", "balance", "poly-usdc"]).output().unwrap().stdout);
    assert_eq!(v["balance"], "0.000000");

    let v = json(&ledger(&db).args(["--json", "import"]).write_stdin(lines).output().unwrap().stdout);
    assert_eq!(v["imported"], 2);
    let v = json(&ledger(&db).args(["--json", "import"]).write_stdin(lines).output().unwrap().stdout);
    assert_eq!(v["duplicates"], 2);

    let bad = "{\"account\":\"poly-usdc\",\"amount\":\"1\",\"kind\":\"deposit\"}\n{\"account\":\"poly-usdc\",\"amount\":1,\"kind\":\"deposit\"}\n";
    let out = ledger(&db).args(["--json", "import"]).write_stdin(bad).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = json(&out.stderr);
    assert_eq!(e["error"]["code"], "invalid_amount");
    assert_eq!(e["error"]["line"], 2);
}

#[test]
fn group_pnl_reconcile_and_snapshots_flow() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    ledger(&db).args(["account", "add", "kalshi-usd", "--currency", "USD"]).assert().success();
    ledger(&db).args(["add", "poly-usdc", "-45", "--kind", "trade", "--group", "arb:1", "--meta", r#"{"strategy":"arb"}"#]).assert().success();
    ledger(&db).args(["add", "kalshi-usd", "-52", "--kind", "trade", "--group", "arb:1", "--meta", r#"{"strategy":"arb"}"#]).assert().success();
    ledger(&db).args(["add", "poly-usdc", "100", "--kind", "settlement", "--group", "arb:1", "--ref", "settle:m1"]).assert().success();

    let v = json(&ledger(&db).args(["--json", "group", "arb:1"]).output().unwrap().stdout);
    assert_eq!(v["entries"].as_array().unwrap().len(), 3);
    assert_eq!(v["net"]["USDC"], "55.000000");
    assert_eq!(v["net"]["USD"], "-52.00");

    let v = json(&ledger(&db).args(["--json", "pnl", "--by", "meta:strategy"]).output().unwrap().stdout);
    assert_eq!(v["accounts"][0]["account"], "kalshi-usd");
    assert_eq!(v["accounts"][0]["rows"][0]["bucket"], "arb");
    assert_eq!(v["accounts"][0]["rows"][0]["net"], "-52.00");
    assert_eq!(v["accounts"][1]["rows"][0]["net"], "55.000000");

    let v = json(&ledger(&db).args(["--json", "reconcile", "poly-usdc", "--observed", "54.5", "--source", "chain"]).output().unwrap().stdout);
    assert_eq!(v["snapshot"]["diff"], "-0.500000");
    assert_eq!(v["adjustment"]["kind"], "adjustment");
    let v = json(&ledger(&db).args(["--json", "snapshots", "poly-usdc"]).output().unwrap().stdout);
    assert_eq!(v["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(v["snapshots"][0]["source"], "chain");

    let v = json(&ledger(&db).args(["--json", "reverse", "--group", "arb:1"]).output().unwrap().stdout);
    assert_eq!(v["entries"].as_array().unwrap().len(), 3);
    let v = json(&ledger(&db).args(["--json", "show", "1"]).output().unwrap().stdout);
    assert!(v["entry"]["reversed_by"].is_number());

    let v = json(&ledger(&db).args(["--json", "transfer", "poly-usdc", "kalshi-usd", "1"]).output().unwrap().stderr);
    assert_eq!(v["error"]["code"], "currency_mismatch");
}

#[test]
fn ledger_db_env_and_default_dir_creation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("deep").join("l.db");
    let mut c = Command::cargo_bin("ledger").unwrap();
    c.env("LEDGER_DB", &db).env_remove("LEDGER_ACTOR");
    c.args(["account", "add", "w", "--currency", "USD"]).assert().success();
    assert!(db.exists());
}

#[test]
fn export_csv_header_and_json_shape() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    ledger(&db).args(["add", "poly-usdc", "1", "--kind", "deposit", "--memo", "a, \"quoted\""]).assert().success();
    let out = ledger(&db).args(["export", "poly-usdc", "--format", "csv"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next().unwrap(), "id,ts,recorded_at,kind,amount,balance_after,ref,memo,actor,group_id,meta,reverses_id,reversed_by");
    assert!(lines.next().unwrap().contains("\"a, \"\"quoted\"\"\""));
    let v = json(&ledger(&db).args(["export", "poly-usdc", "--format", "json"]).output().unwrap().stdout);
    assert_eq!(v["entries"][0]["balance_after"], "1.000000");
}
```

The `export --format json` test passes without `--json` because export chooses its own format; make `run` return `Output::History` and have `main` print JSON when the command is `export --format json` — implement this by checking in `main`: `let json = cli.json || matches!(&cli.command, Command::Export(e) if matches!(e.format, ExportFormat::Json));` before `run(cli)`.

- [ ] **Step 6: Run to verify failure, then fix until green**

Run: `cargo test --test cli`
Expected: `export_csv_header_and_json_shape` fails on the CSV header (render is stubbed); every other test passes. Leave that one red — Task 12 turns it green. Run `cargo test` and confirm the only failure is that CSV assertion.

- [ ] **Step 7: fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings`
Expected: clean.

```bash
git add src/main.rs src/cli tests/cli.rs
git commit -m "feat: clap CLI with JSON output and exit codes"
```

---

### Task 12: Human-readable tables and CSV

**Files:**
- Modify: `src/cli/render.rs`

**Interfaces:**
- Produces: `render(&Output) -> String` (text tables), `csv(&History) -> String`.

- [ ] **Step 1: Write failing unit tests in `render.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_aligns_columns_and_right_aligns_numbers() {
        let t = table(&["id", "amount"], &[vec!["1".into(), "-25.50".into()], vec!["12".into(), "100.00".into()]], &[1]);
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test render`
Expected: compile error (`table`, `csv_field` missing).

- [ ] **Step 3: Implement render.rs**

```rust
use agent_ledger::model::*;

use super::output::Output;

pub fn render(out: &Output) -> String {
    match out {
        Output::Account { account } => format!(
            "account {} ({}, {} decimals) id={}\n",
            account.name, account.currency, account.decimals, account.id
        ),
        Output::Accounts { accounts } => table(
            &["id", "name", "currency", "decimals", "note"],
            &accounts.iter().map(|a| vec![
                a.id.to_string(), a.name.clone(), a.currency.clone(), a.decimals.to_string(), opt(&a.note),
            ]).collect::<Vec<_>>(),
            &[0, 3],
        ),
        Output::Add(r) => {
            let mut s = entries_table(std::slice::from_ref(&r.entry), false);
            s.push_str(&format!("balance {} {}{}\n", r.balance, r.entry.currency, if r.duplicate { "  (duplicate: entry already existed)" } else { "" }));
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
            &["account", "currency", "balance", "entries", "last_ts", "last_reconciled_at"],
            &accounts.iter().map(|b| vec![
                b.account.clone(), b.currency.clone(), b.balance.clone(), b.entries.to_string(),
                opt(&b.last_ts), opt(&b.last_reconciled_at),
            ]).collect::<Vec<_>>(),
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
        Output::Pnl { accounts } => {
            let mut s = String::new();
            for a in accounts {
                s.push_str(&format!("{} ({})\n", a.account, a.currency));
                s.push_str(&table(
                    &["bucket", "trades", "settlements", "fees", "adjustments", "other", "net"],
                    &a.rows.iter().map(|r| vec![
                        r.bucket.clone().unwrap_or_else(|| "total".into()), r.trades.clone(), r.settlements.clone(),
                        r.fees.clone(), r.adjustments.clone(), r.other.clone(), r.net.clone(),
                    ]).collect::<Vec<_>>(),
                    &[1, 2, 3, 4, 5, 6],
                ));
            }
            s
        }
        Output::Reconcile(r) => {
            let s = &r.snapshot;
            let mut out = format!(
                "snapshot {} {}: observed {} book {} diff {}{}\n",
                s.id, s.ts, s.observed, s.book, s.diff,
                s.source.as_ref().map(|src| format!(" ({src})")).unwrap_or_default()
            );
            match &r.adjustment {
                Some(e) => out.push_str(&entries_table(std::slice::from_ref(e), false)),
                None => out.push_str("no adjustment posted\n"),
            }
            out
        }
        Output::Snapshots(l) => table(
            &["id", "ts", "observed", "book", "diff", "adjustment", "source"],
            &l.snapshots.iter().map(|s| vec![
                s.id.to_string(), s.ts.clone(), s.observed.clone(), s.book.clone(), s.diff.clone(),
                s.adjustment_entry_id.map(|v| v.to_string()).unwrap_or_default(), opt(&s.source),
            ]).collect::<Vec<_>>(),
            &[0, 2, 3, 4, 5],
        ),
        Output::Import(r) => {
            let mut s = format!(
                "imported {} duplicates {}{}\n",
                r.imported, r.duplicates, if r.dry_run { "  (dry run, nothing written)" } else { "" }
            );
            s.push_str(&entries_table(&r.entries, true));
            s
        }
        Output::Raw(text) => text.clone(),
    }
}

pub fn csv(history: &History) -> String {
    let mut out = String::from(
        "id,ts,recorded_at,kind,amount,balance_after,ref,memo,actor,group_id,meta,reverses_id,reversed_by\n",
    );
    for h in &history.entries {
        let e = &h.entry;
        let fields = [
            e.id.to_string(), e.ts.clone(), e.recorded_at.clone(), e.kind.as_str().to_string(), e.amount.clone(),
            h.balance_after.clone(), opt(&e.reference), opt(&e.memo), opt(&e.actor), opt(&e.group_id),
            e.meta.as_ref().map(|m| m.to_string()).unwrap_or_default(),
            e.reverses_id.map(|v| v.to_string()).unwrap_or_default(),
            e.reversed_by.map(|v| v.to_string()).unwrap_or_default(),
        ];
        out.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
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
    headers.extend(["ts", "kind", "amount", "ref", "group", "memo", "actor", "reversal"]);
    let rows = entries.iter().map(|e| {
        let mut row = vec![e.id.to_string()];
        if with_account {
            row.push(e.account.clone());
        }
        row.extend([
            e.ts.clone(), e.kind.as_str().to_string(), e.amount.clone(), opt(&e.reference),
            opt(&e.group_id), opt(&e.memo), opt(&e.actor), rev(e),
        ]);
        row
    }).collect::<Vec<_>>();
    let amount_col = if with_account { 4 } else { 3 };
    table(&headers, &rows, &[0, amount_col])
}

fn history_table(h: &History) -> String {
    let mut s = format!("{} ({})\n", h.account, h.currency);
    let rows = h.entries.iter().map(|x| {
        let e = &x.entry;
        vec![
            e.id.to_string(), e.ts.clone(), e.kind.as_str().to_string(), e.amount.clone(), x.balance_after.clone(),
            opt(&e.reference), opt(&e.group_id), opt(&e.memo), opt(&e.actor), rev(e),
        ]
    }).collect::<Vec<_>>();
    s.push_str(&table(
        &["id", "ts", "kind", "amount", "balance", "ref", "group", "memo", "actor", "reversal"],
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
        for i in 0..cols {
            let cell = cells.get(i).map(String::as_str).unwrap_or("");
            let pad = widths[i] - cell.chars().count();
            if right.contains(&i) {
                parts.push(format!("{}{}", " ".repeat(pad), cell));
            } else {
                parts.push(format!("{}{}", cell, " ".repeat(pad)));
            }
        }
        parts.join("  ").trim_end().to_string()
    };
    let mut out = String::new();
    out.push_str(&fmt_row(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>()));
    out.push('\n');
    out.push_str(&fmt_row(&widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>()));
    out.push('\n');
    for row in rows {
        out.push_str(&fmt_row(row));
        out.push('\n');
    }
    out
}
```

Note `right` for the id column in `table_aligns_columns_and_right_aligns_numbers` is `&[1]` only, so `1 ` stays left-aligned there; the test's expected strings reflect that.

- [ ] **Step 4: Run everything, fmt, clippy, commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green, including the CSV assertion from Task 11.

```bash
git add src/cli/render.rs
git commit -m "feat: text tables and csv export rendering"
```

---

### Task 13: Companion skill, Makefile, docs, final verification

**Files:**
- Create: `skill/ledger/SKILL.md`, `Makefile`
- Modify: `README.md` (status → usable, install instructions), `docs/specs/2026-09-06-agent-ledger-design.md` (error code list gains `invalid_account_name`, `invalid_currency`, `same_account`, `invalid_bucket`, `invalid_json`, `database_error`, `io_error`; `--by` default `total`; reconcile inserts the adjustment before the snapshot because snapshots are append-only)

- [ ] **Step 1: Write the skill**

Load `superpowers:writing-skills` for format rules, then write `skill/ledger/SKILL.md`:

```markdown
---
name: ledger
description: Use when an agent moves money, records a trade fill, fee or settlement, checks a balance, needs realized PnL, or has just fetched a live balance from an exchange or chain - drives the append-only `ledger` CLI so every movement is on the books
---

# Ledger

`ledger` is an append-only SQLite ledger. One binary, one file, exact money, JSON output.
It records cash movements per account and reconciles them against what the venue reports.
It does not value open positions; combine `ledger balance` with venue positions for equity.

## When to use

- You are about to deposit, withdraw, buy, sell, redeem, or pay a fee: record it right after it happens.
- You just fetched a live balance from a venue or chain: `reconcile` immediately.
- You need to know a balance, what happened, or realized PnL for a period, strategy, or position.
- You are backfilling history: `import` JSON Lines.

## Conventions

1. Always pass `--json` and parse stdout. Errors are on stderr as `{"error":{"code","message"}}`; exit 0 ok, 1 usage/IO, 2 domain error.
2. Always pass `--ref` when an external id exists (order id, tx hash, fill id). Replaying the same ref is a safe no-op (`duplicate: true`). No venue id? Build a deterministic one: `settle:<market>`, `fee:<order-id>`.
3. Sign: positive is money into the account, negative is money out. `deposit` positive; `withdrawal` and `fee` negative; `trade` buy negative, sell positive; `settlement` is cash from a resolution, redeem, expiry, or funding.
4. Round to the account's decimals yourself. The ledger rejects extra precision instead of rounding.
5. One ledger account per real venue balance (`poly-usdc`, `kalshi-usd`), so `reconcile` compares like with like. Put the strategy in `--meta '{"strategy":"..."}'`, not in the account name.
6. Every leg of one position gets the same `--group`. A cross-venue arbitrage has two trade legs and up to two settlement legs under one group; `ledger group <id>` shows the net per currency.
7. USDC on Polygon to USD at Kalshi is a `withdrawal` on one account and a `deposit` on the other, optionally sharing a `--group`. `transfer` is only for same-currency accounts.
8. Never open the SQLite file directly. Fix mistakes with `reverse <id>` or `reverse --group <id>`; the database refuses updates and deletes.
9. Set `LEDGER_ACTOR` (or `--actor`) to your agent name so the audit trail says who wrote each row.

Meta keys, all strings: `market`, `side`, `price`, `shares`, `strategy`, `venue`.

## Commands

| task | command |
|---|---|
| create account | `ledger account add poly-usdc --currency USDC --decimals 6` |
| record movement | `ledger add <account> <amount> --kind <deposit|withdrawal|trade|settlement|fee|adjustment|other> [--ref ID] [--group G] [--meta JSON] [--memo TEXT] [--ts RFC3339]` |
| same-currency move | `ledger transfer <from> <to> <amount> [--ref ID] [--group G]` |
| undo | `ledger reverse <entry-id>` or `ledger reverse --group <id>` |
| balances | `ledger balance` / `ledger balance <account> [--at TS]` |
| history | `ledger history <account> [--limit N] [--since TS] [--until TS] [--kind K] [--group G]` |
| position view | `ledger group <id>` |
| realized pnl | `ledger pnl [<account>] [--since TS] [--until TS] [--by day|week|month|group|meta:<key>]` |
| reconcile | `ledger reconcile <account> --observed <amount> [--source TEXT] [--no-adjust] [--ts TS]` |
| past reconciles | `ledger snapshots <account>` |
| backfill | `cat rows.jsonl \| ledger import [--dry-run]` |
| inspect | `ledger show <entry-id>` |
| dump | `ledger export <account> --format csv\|json` |

Every command takes `--json`. `--db PATH` or `LEDGER_DB` picks the file; default `~/.agent-ledger/ledger.db`.

## Worked flow: Polymarket wallet

```sh
export LEDGER_ACTOR=claude
ledger account add poly-usdc --currency USDC --decimals 6 --json
ledger add poly-usdc 100 --kind deposit --ref 0x9f3…c1 --memo "bridge in" --json
ledger add poly-usdc -25.500000 --kind trade --ref ord-7f3 --group btc-5m-0310 \
  --meta '{"market":"btc-5m-0310","side":"buy","price":"0.51","shares":"50","strategy":"momentum","venue":"polymarket"}' --json
ledger add poly-usdc 50 --kind settlement --ref settle:btc-5m-0310 --group btc-5m-0310 --json
ledger add poly-usdc -0.892500 --kind fee --ref fee:ord-7f3 --group btc-5m-0310 --json
ledger group btc-5m-0310 --json            # net USDC for the position
ledger reconcile poly-usdc --observed 123.607500 --source polymarket-onchain --json
ledger pnl poly-usdc --since 2026-09-01 --by meta:strategy --json
```

## Worked flow: two-venue arbitrage

```sh
G=arb:btc-5m-0310
ledger add poly-usdc  -45 --kind trade --ref p-o1 --group $G --meta '{"side":"buy","market":"btc-5m-0310","strategy":"arb","venue":"polymarket"}' --json
ledger add kalshi-usd -52 --kind trade --ref k-o1 --group $G --meta '{"side":"buy","market":"KXBTC-0310","strategy":"arb","venue":"kalshi"}' --json
# resolution: Polymarket side pays out, Kalshi side expires worthless (no entry needed)
ledger add poly-usdc 100 --kind settlement --ref settle:btc-5m-0310 --group $G --json
ledger group $G --json                     # net: {"USDC":"55.000000","USD":"-52.00"}
```

## Import format

One JSON object per line. `amount` must be a string. Only `add` kinds are importable.

```json
{"account":"poly-usdc","amount":"-25.500000","kind":"trade","ref":"ord-7f3","ts":"2026-09-06T03:10:02Z","group":"btc-5m-0310","memo":"buy","meta":{"market":"btc-5m-0310","side":"buy"}}
```

Run with `--dry-run` first; any error rolls back the whole batch and names the line.

## Mistakes to avoid

- Posting a fee as positive, or a deposit as negative: the ledger rejects it (`invalid_sign`).
- Recording one arbitrage leg without `--group`: the position can no longer be netted.
- Reconciling before recording known fills: the diff becomes an adjustment that hides the real cause.
- Summing `net` across currencies: there is no FX; report per currency.
```

- [ ] **Step 2: Write the Makefile**

```makefile
SKILL_DIR ?= $(HOME)/.claude/skills

.PHONY: build test install uninstall

build:
	cargo build --release

test:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

install:
	cargo install --path . --locked
	mkdir -p $(SKILL_DIR)
	ln -sfn $(CURDIR)/skill/ledger $(SKILL_DIR)/ledger
	@echo "installed: $$(command -v ledger) and $(SKILL_DIR)/ledger -> $(CURDIR)/skill/ledger"

uninstall:
	cargo uninstall agent-ledger
	rm -f $(SKILL_DIR)/ledger
```

- [ ] **Step 3: Update README and spec**

README: change the status paragraph to "Status: v0.1, usable. `make install` builds the binary and links the skill." Add an Install section:

```markdown
## Install

Requires a Rust toolchain.

```sh
git clone https://github.com/newbdez33/agent-ledger && cd agent-ledger
make install          # cargo install + symlink skill/ledger into ~/.claude/skills/ledger
ledger --help
```
```

Spec: extend the domain error code list, note `--by` default `total`, and rewrite the reconcile rule's last sentence to "the adjustment entry is inserted first so the snapshot can reference it, because snapshots are append-only".

- [ ] **Step 4: Full verification**

Run:

```bash
make test
cargo build --release
tmp=$(mktemp -d) && LEDGER_DB=$tmp/l.db ./target/release/ledger account add w --currency USD \
  && LEDGER_DB=$tmp/l.db ./target/release/ledger add w 10 --kind deposit --json \
  && LEDGER_DB=$tmp/l.db ./target/release/ledger balance
```

Expected: fmt clean, clippy clean, all tests pass, the smoke run prints a JSON entry then a balance table showing `10.00`.

- [ ] **Step 5: Commit and push**

```bash
git add skill Makefile README.md docs/specs/2026-09-06-agent-ledger-design.md
git commit -m "feat: companion skill, make install, docs for v0.1"
git push origin main
```

---

## Self-review

**Spec coverage.** Storage/path/migration → Task 3, 11. Money/time → Task 1, 2. Schema and triggers → Task 3. Kinds and signs → Task 2. Idempotent add → Task 4, 5. Group and meta → Task 5, 7. Transfer → Task 6. Reversal (single, sibling, group, inherit group) → Task 6. Reconcile as-of and snapshots → Task 9. Balance/history/export → Task 7. PnL → Task 8. Import → Task 10. CLI, output shapes, errors and exit codes → Task 11, 12. Skill and Makefile → Task 13. Testing list → each task's tests plus `tests/cli.rs`.

**Placeholder scan.** No TBD/TODO. Every step carries code.

**Type consistency.** `AddRequest`/`TransferRequest` live in `entries.rs` and are re-exported from `ledger`; `HistoryFilter` from `reports.rs`; `PnlBucket`/`PnlFilter` from `pnl.rs`; `ReconcileRequest` from `reconcile.rs`; all result structs in `model.rs`. `main.rs` imports exactly those paths. `entry_from_row` column order is fixed by `ENTRY_SELECT` and reused verbatim in `history` with `balance_after` at index 15.
