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
    CurrencyMismatch {
        from: String,
        from_currency: String,
        to: String,
        to_currency: String,
    },
    #[error("amount {amount} has {scale} decimals; account '{account}' allows {decimals}")]
    PrecisionExceeded {
        amount: String,
        scale: u32,
        account: String,
        decimals: u32,
    },
    #[error("invalid amount '{0}'")]
    InvalidAmount(String),
    #[error("amount must not be zero")]
    ZeroAmount,
    #[error("{kind} must be {expected}, got {amount}")]
    InvalidSign {
        kind: String,
        expected: &'static str,
        amount: String,
    },
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
    RefConflict {
        account: String,
        reference: String,
        existing_id: i64,
        existing_kind: String,
        existing_amount: String,
    },
    #[error("entry {0} is already reversed by entry {1}")]
    AlreadyReversed(i64, i64),
    #[error("entry {0} is a reversal; add the original again instead of reversing it")]
    CannotReverseReversal(i64),
    #[error("nothing to reverse in group '{0}'")]
    NothingToReverse(String),
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("line {line}: {source}")]
    Import {
        line: usize,
        #[source]
        source: Box<LedgerError>,
    },
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
        let e = LedgerError::Import {
            line: 3,
            source: Box::new(LedgerError::InvalidGroup),
        };
        assert_eq!(e.code(), "invalid_group");
        assert_eq!(e.line(), Some(3));
        assert_eq!(e.exit_code(), 2);
        assert_eq!(e.to_string(), "line 3: group id must not be empty");
    }
}
