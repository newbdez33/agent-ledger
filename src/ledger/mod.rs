//! The `Ledger` type and row-level helpers shared by every operation.

// Helpers land before their callers; removed once import (Task 10) is in.
#![allow(dead_code)]

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
        Ok(Self {
            conn: db::open(path)?,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self {
            conn: db::open_in_memory()?,
        })
    }

    pub(crate) fn write_tx(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
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
    let mut stmt = conn.prepare(
        "SELECT id, name, currency, decimals FROM accounts ORDER BY name COLLATE NOCASE",
    )?;
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
        Some(t) => {
            time::normalize(t).ok_or_else(|| LedgerError::InvalidTimestamp(t.trim().to_string()))
        }
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
    reference
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
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

pub(crate) fn existing_ref(
    conn: &Connection,
    account_id: i64,
    reference: &str,
) -> Result<Option<ExistingRef>> {
    Ok(conn
        .query_row(
            "SELECT id, kind, amount FROM entries WHERE account_id = ?1 AND ref = ?2",
            params![account_id, reference],
            |r| {
                Ok(ExistingRef {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    amount: r.get(2)?,
                })
            },
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
            e.account.id,
            e.ts,
            time::now(),
            e.kind.as_str(),
            e.amount,
            e.reference,
            e.memo,
            e.actor,
            e.group_id,
            e.meta,
            e.reverses_id
        ],
    )?;
    Ok(Written::Inserted(conn.last_insert_rowid()))
}

/// Column order: 0 id, 1 account, 2 currency, 3 decimals, 4 ts, 5 recorded_at, 6 kind,
/// 7 amount, 8 ref, 9 memo, 10 actor, 11 group_id, 12 meta, 13 reverses_id, 14 reversed_by.
pub(crate) const ENTRY_SELECT: &str =
    "SELECT e.id, a.name, a.currency, a.decimals, e.ts, e.recorded_at, e.kind, e.amount, \
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
    conn.query_row(
        &format!("{ENTRY_SELECT} WHERE e.id = ?1"),
        params![id],
        entry_from_row,
    )
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
