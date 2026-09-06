use rusqlite::{params, Connection, Row};

use super::{account_by_name, Ledger};
use crate::error::{LedgerError, Result};
use crate::model::{Account, AccountAddResult};
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
    Ok(conn.query_row(
        &format!("{ACCOUNT_SELECT} WHERE id = ?1"),
        params![id],
        account_from_row,
    )?)
}

impl Ledger {
    pub fn add_account(
        &mut self,
        name: &str,
        currency: &str,
        decimals: u32,
        note: Option<&str>,
    ) -> Result<AccountAddResult> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LedgerError::InvalidAccountName);
        }
        let currency = currency.trim().to_uppercase();
        if currency.is_empty() {
            return Err(LedgerError::InvalidCurrency);
        }
        let tx = self.write_tx()?;
        if let Ok(existing) = account_by_name(&tx, name) {
            if existing.currency == currency && existing.decimals == decimals {
                let account = load_account(&tx, existing.id)?;
                return Ok(AccountAddResult {
                    account,
                    duplicate: true,
                });
            }
            return Err(LedgerError::AccountExists(name.to_string()));
        }
        tx.execute(
            "INSERT INTO accounts (name, currency, decimals, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, currency, decimals as i64, note.map(str::trim), time::now()],
        )?;
        let id = tx.last_insert_rowid();
        let account = load_account(&tx, id)?;
        tx.commit()?;
        Ok(AccountAddResult {
            account,
            duplicate: false,
        })
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{ACCOUNT_SELECT} ORDER BY name COLLATE NOCASE"))?;
        let rows = stmt.query_map([], account_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_and_lists_with_uppercase_currency() {
        let mut l = Ledger::open_in_memory().unwrap();
        let a = l
            .add_account(" poly-usdc ", "usdc", 6, Some("polymarket proxy wallet"))
            .unwrap()
            .account;
        assert_eq!(a.name, "poly-usdc");
        assert_eq!(a.currency, "USDC");
        assert_eq!(a.decimals, 6);
        assert_eq!(a.note.as_deref(), Some("polymarket proxy wallet"));
        l.add_account("kalshi-usd", "USD", 2, None).unwrap();
        let names: Vec<String> = l
            .list_accounts()
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, vec!["kalshi-usd", "poly-usdc"]);
    }

    #[test]
    fn identical_readd_is_duplicate_including_case_insensitive_name() {
        let mut l = Ledger::open_in_memory().unwrap();
        let first = l.add_account("Wallet", "USD", 2, Some("kept")).unwrap();
        assert!(!first.duplicate);
        let again = l.add_account("wallet", "usd", 2, Some("ignored")).unwrap();
        assert!(again.duplicate);
        assert_eq!(again.account.id, first.account.id);
        assert_eq!(again.account.name, "Wallet");
        assert_eq!(again.account.note.as_deref(), Some("kept"));
    }

    #[test]
    fn different_currency_or_decimals_is_still_exists() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_account("poly-usdc", "USDC", 6, None).unwrap();
        assert!(matches!(
            l.add_account("poly-usdc", "USD", 6, None),
            Err(LedgerError::AccountExists(_))
        ));
        assert!(matches!(
            l.add_account("poly-usdc", "USDC", 2, None),
            Err(LedgerError::AccountExists(_))
        ));
    }

    #[test]
    fn rejects_empty_name_or_currency() {
        let mut l = Ledger::open_in_memory().unwrap();
        assert!(matches!(
            l.add_account("  ", "USD", 2, None),
            Err(LedgerError::InvalidAccountName)
        ));
        assert!(matches!(
            l.add_account("x", " ", 2, None),
            Err(LedgerError::InvalidCurrency)
        ));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_account("Poly-USDC", "USDC", 6, None).unwrap();
        let row = account_by_name(&l.conn, "poly-usdc").unwrap();
        assert_eq!(row.name, "Poly-USDC");
        assert!(matches!(
            account_by_name(&l.conn, "nope"),
            Err(LedgerError::AccountNotFound(_))
        ));
    }
}
