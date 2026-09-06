//! Storage timestamp format: RFC 3339 UTC with milliseconds and a literal Z.

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};

pub fn now() -> String {
    to_storage(Utc::now())
}

/// Accepts RFC 3339 with any offset, or a bare `YYYY-MM-DD` (UTC midnight).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_offset_to_utc_millis() {
        assert_eq!(
            normalize("2026-09-06T12:00:00+09:00").unwrap(),
            "2026-09-06T03:00:00.000Z"
        );
    }

    #[test]
    fn accepts_date_only_as_utc_midnight() {
        assert_eq!(
            normalize(" 2026-09-01 ").unwrap(),
            "2026-09-01T00:00:00.000Z"
        );
    }

    #[test]
    fn keeps_millis() {
        assert_eq!(
            normalize("2026-09-06T03:12:45.117Z").unwrap(),
            "2026-09-06T03:12:45.117Z"
        );
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
