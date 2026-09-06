//! Exact money: decimal text at the edges, i64 minor units inside.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Debug, PartialEq, Eq)]
pub enum AmountError {
    Invalid,
    Precision { scale: u32 },
}

/// Parse a decimal string into minor units for an account with `decimals` fraction digits.
/// Excess precision is rejected, never rounded.
pub fn parse_amount(text: &str, decimals: u32) -> Result<i64, AmountError> {
    let trimmed = text.trim();
    let unsigned = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let digits = unsigned.strip_prefix('-').unwrap_or(unsigned);
    // Only plain decimal notation: rust_decimal would otherwise accept "1e5".
    if digits.is_empty()
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        || !digits.chars().any(|c| c.is_ascii_digit())
    {
        return Err(AmountError::Invalid);
    }
    let value = Decimal::from_str(unsigned)
        .map_err(|_| AmountError::Invalid)?
        .normalize();
    if value.scale() > decimals {
        return Err(AmountError::Precision {
            scale: value.scale(),
        });
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
    format!(
        "{sign}{}.{:0width$}",
        abs / base,
        abs % base,
        width = decimals as usize
    )
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
        assert_eq!(
            parse_amount("1.2345678", 6),
            Err(AmountError::Precision { scale: 7 })
        );
        assert_eq!(
            parse_amount("0.001", 2),
            Err(AmountError::Precision { scale: 3 })
        );
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
