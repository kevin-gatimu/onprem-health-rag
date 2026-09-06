//! Time-phrase parser for the v3 intent router.
//!
//! Maps natural-language time expressions to [`TimeRange`] values from the IR.
//! **Never reads the system clock** — callers inject `now: DateTime<Utc>` so
//! golden tests with a frozen timestamp stay valid across days (plan 03a §0.3).

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};

use crate::nl2sql::ir::spec::{BucketUnit, FilterValue, TimeRange};

/// Parse a natural-language time phrase out of `text`.  Returns the first
/// recognised phrase as a `TimeRange`, or `None` when no phrase is found.
///
/// `now` must be provided by the caller — never read the clock here.
pub fn parse_time(text: &str, now: DateTime<Utc>) -> Option<TimeRange> {
    let q = text.to_ascii_lowercase();
    let today = now.date_naive();

    // Named exact-match variants (cheapest, no arithmetic).
    if contains_any(&q, &["today", "this day"]) {
        return Some(TimeRange::Today);
    }
    if contains_any(&q, &["yesterday"]) {
        return Some(TimeRange::Yesterday);
    }
    if contains_any(&q, &["this week", "current week"]) {
        return Some(TimeRange::ThisWeek);
    }
    if contains_any(&q, &["last week", "previous week"]) {
        return Some(TimeRange::LastWeek);
    }
    if contains_any(&q, &["this month", "current month"]) {
        return Some(TimeRange::ThisMonth);
    }
    if contains_any(&q, &["last month", "previous month"]) {
        return Some(TimeRange::LastMonth);
    }
    if contains_any(&q, &["this quarter", "current quarter"]) {
        return Some(TimeRange::ThisQuarter);
    }
    if contains_any(&q, &["last quarter", "previous quarter"]) {
        return Some(TimeRange::LastQuarter);
    }
    if contains_any(&q, &["year to date", "ytd", "this year", "current year"]) {
        // Year-to-date = start of year → today
        let lo = NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap();
        return Some(TimeRange::Absolute {
            lo: FilterValue::Date(lo),
            hi: FilterValue::Date(today),
        });
    }
    if contains_any(&q, &["last year", "previous year"]) {
        return Some(TimeRange::LastYear);
    }

    // Shift-based windows (before numeric N-day patterns to avoid false hits).
    if contains_any(&q, &["night shift", "on nights", "overnight"]) {
        // Night shift: previous calendar day 19:00 → today 07:00.
        let prev = today - Duration::days(1);
        let lo = prev
            .and_hms_opt(19, 0, 0)
            .unwrap()
            .and_utc();
        let hi = today
            .and_hms_opt(7, 0, 0)
            .unwrap()
            .and_utc();
        return Some(TimeRange::Absolute {
            lo: FilterValue::Ts(lo),
            hi: FilterValue::Ts(hi),
        });
    }
    if contains_any(&q, &["day shift", "morning shift"]) {
        // Day shift: today 08:00 → today 16:00.
        let lo = today.and_hms_opt(8, 0, 0).unwrap().and_utc();
        let hi = today.and_hms_opt(16, 0, 0).unwrap().and_utc();
        return Some(TimeRange::Absolute {
            lo: FilterValue::Ts(lo),
            hi: FilterValue::Ts(hi),
        });
    }

    // Quarter names: Q1/Q2/Q3/Q4 (current year).
    if let Some(qn) = quarter_name(&q) {
        let (m_start, m_end) = match qn {
            1 => (1u32, 3u32),
            2 => (4, 6),
            3 => (7, 9),
            _ => (10, 12),
        };
        let year = today.year();
        let lo = NaiveDate::from_ymd_opt(year, m_start, 1).unwrap();
        let hi = last_day_of_month(year, m_end);
        return Some(TimeRange::Absolute {
            lo: FilterValue::Date(lo),
            hi: FilterValue::Date(hi),
        });
    }

    // "in YYYY" → full calendar year.
    if let Some(year) = extract_year(&q) {
        let lo = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
        let hi = NaiveDate::from_ymd_opt(year, 12, 31).unwrap();
        return Some(TimeRange::Absolute {
            lo: FilterValue::Date(lo),
            hi: FilterValue::Date(hi),
        });
    }

    // "between <month> and <month>" — same calendar year as `now`.
    if let Some((m1, m2)) = extract_month_range(&q) {
        let year = today.year();
        let lo = NaiveDate::from_ymd_opt(year, m1, 1).unwrap();
        let hi = last_day_of_month(year, m2);
        return Some(TimeRange::Absolute {
            lo: FilterValue::Date(lo),
            hi: FilterValue::Date(hi),
        });
    }

    // Single month name → current year.
    if let Some(m) = extract_single_month(&q) {
        let year = today.year();
        let lo = NaiveDate::from_ymd_opt(year, m, 1).unwrap();
        let hi = last_day_of_month(year, m);
        return Some(TimeRange::Absolute {
            lo: FilterValue::Date(lo),
            hi: FilterValue::Date(hi),
        });
    }

    // "within N <unit>" — e.g. "within 7 days", "within 30 days".
    if let Some((n, unit)) = extract_n_unit(&q, &["within"]) {
        return Some(TimeRange::Within { n, unit });
    }

    // "next N <unit>".
    if let Some((n, unit)) = extract_n_unit(&q, &["next"]) {
        return Some(TimeRange::Next { n, unit });
    }

    // "last N <unit>" / "past N <unit>" / "over the past N <unit>".
    if let Some((n, unit)) = extract_n_unit(&q, &["last", "past", "previous", "over the past"]) {
        return Some(TimeRange::Last { n, unit });
    }

    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Last day of month in a given year.
fn last_day_of_month(year: i32, month: u32) -> NaiveDate {
    let next_month = if month == 12 { 1 } else { month + 1 };
    let next_year = if month == 12 { year + 1 } else { year };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .unwrap()
        .pred_opt()
        .unwrap()
}

/// Detect "Q1", "Q2", "Q3", "Q4" (case-insensitive) anywhere in the string.
fn quarter_name(q: &str) -> Option<u32> {
    for (kw, n) in &[("q1", 1u32), ("q2", 2), ("q3", 3), ("q4", 4)] {
        if q.contains(kw) {
            return Some(*n);
        }
    }
    None
}

/// Extract a 4-digit calendar year preceded by "in " (e.g. "in 2025").
fn extract_year(q: &str) -> Option<i32> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\bin\s+(20\d{2})\b").expect("year regex")
    });
    RE.captures(q)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Extract "between <month> and <month>", returning (start_month, end_month) as 1-12.
fn extract_month_range(q: &str) -> Option<(u32, u32)> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"\bbetween\s+(\w+)\s+and\s+(\w+)",
        )
        .expect("month-range regex")
    });
    let caps = RE.captures(q)?;
    let m1 = parse_month_name(caps.get(1)?.as_str())?;
    let m2 = parse_month_name(caps.get(2)?.as_str())?;
    Some((m1, m2))
}

/// Extract a single month name not preceded by "between…and" nor "and" (to avoid
/// double-counting the second month in a range).
fn extract_single_month(q: &str) -> Option<u32> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"\b(january|february|march|april|may|june|july|august|september|october|november|december)\b",
        )
        .expect("month regex")
    });
    // Only return if there's exactly one match (multi-month phrases handled above).
    let matches: Vec<_> = RE.find_iter(q).collect();
    if matches.len() == 1 {
        parse_month_name(matches[0].as_str())
    } else {
        None
    }
}

/// Parse "last N <unit>", "next N <unit>", "within N <unit>", etc.
/// Returns (N, BucketUnit) when a prefix keyword followed by a number and a unit
/// are found.
fn extract_n_unit(q: &str, prefixes: &[&str]) -> Option<(u32, BucketUnit)> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"\b(\d+)\s+(day|days|week|weeks|month|months|year|years|quarter|quarters|hour|hours)\b",
        )
        .expect("n-unit regex")
    });
    // Only extract when one of the prefix keywords precedes the number.
    for prefix in prefixes {
        if !q.contains(prefix) {
            continue;
        }
        if let Some(cap) = RE.captures(q) {
            let n: u32 = cap.get(1)?.as_str().parse().ok()?;
            let unit = parse_bucket_unit(cap.get(2)?.as_str())?;
            return Some((n, unit));
        }
    }
    None
}

fn parse_month_name(s: &str) -> Option<u32> {
    match s {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

fn parse_bucket_unit(s: &str) -> Option<BucketUnit> {
    match s {
        "hour" | "hours" => Some(BucketUnit::Hour),
        "day" | "days" => Some(BucketUnit::Day),
        "week" | "weeks" => Some(BucketUnit::Week),
        "month" | "months" => Some(BucketUnit::Month),
        "quarter" | "quarters" => Some(BucketUnit::Quarter),
        "year" | "years" => Some(BucketUnit::Year),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests — frozen now = 2026-01-15T12:00:00Z
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Fixed reference instant: 2026-01-15 12:00 UTC
    fn frozen_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap()
    }

    fn parse(phrase: &str) -> Option<TimeRange> {
        parse_time(phrase, frozen_now())
    }

    // --- 30 phrases -------------------------------------------------------

    #[test] fn t01_today()        { assert_eq!(parse("how many today"),       Some(TimeRange::Today)); }
    #[test] fn t02_yesterday()    { assert_eq!(parse("yesterday's count"),    Some(TimeRange::Yesterday)); }
    #[test] fn t03_this_week()    { assert_eq!(parse("this week total"),      Some(TimeRange::ThisWeek)); }
    #[test] fn t04_last_week()    { assert_eq!(parse("last week"),            Some(TimeRange::LastWeek)); }
    #[test] fn t05_this_month()   { assert_eq!(parse("this month figure"),    Some(TimeRange::ThisMonth)); }
    #[test] fn t06_last_month()   { assert_eq!(parse("last month"),           Some(TimeRange::LastMonth)); }
    #[test] fn t07_this_quarter() { assert_eq!(parse("this quarter"),         Some(TimeRange::ThisQuarter)); }
    #[test] fn t08_last_quarter() { assert_eq!(parse("last quarter"),         Some(TimeRange::LastQuarter)); }
    #[test] fn t09_this_year()    { assert_eq!(parse("this year"),            Some(TimeRange::Absolute {
        lo: FilterValue::Date(NaiveDate::from_ymd_opt(2026,1,1).unwrap()),
        hi: FilterValue::Date(NaiveDate::from_ymd_opt(2026,1,15).unwrap()),
    })); }
    #[test] fn t10_last_year()    { assert_eq!(parse("last year count"),      Some(TimeRange::LastYear)); }
    #[test] fn t11_ytd()          {
        assert_eq!(parse("year to date"), Some(TimeRange::Absolute {
            lo: FilterValue::Date(NaiveDate::from_ymd_opt(2026,1,1).unwrap()),
            hi: FilterValue::Date(NaiveDate::from_ymd_opt(2026,1,15).unwrap()),
        }));
    }
    #[test] fn t12_last_7_days()  { assert_eq!(parse("last 7 days"),  Some(TimeRange::Last { n:7, unit: BucketUnit::Day })); }
    #[test] fn t13_past_7_days()  { assert_eq!(parse("past 7 days"),  Some(TimeRange::Last { n:7, unit: BucketUnit::Day })); }
    #[test] fn t14_last_30_days() { assert_eq!(parse("last 30 days"), Some(TimeRange::Last { n:30, unit: BucketUnit::Day })); }
    #[test] fn t15_last_3_months(){ assert_eq!(parse("last 3 months"),Some(TimeRange::Last { n:3, unit: BucketUnit::Month })); }
    #[test] fn t16_last_6_months(){ assert_eq!(parse("last 6 months"),Some(TimeRange::Last { n:6, unit: BucketUnit::Month })); }
    #[test] fn t17_last_2_weeks() { assert_eq!(parse("last 2 weeks"), Some(TimeRange::Last { n:2, unit: BucketUnit::Week })); }
    #[test] fn t18_last_4_weeks() { assert_eq!(parse("past 4 weeks"), Some(TimeRange::Last { n:4, unit: BucketUnit::Week })); }
    #[test] fn t19_next_7_days()  { assert_eq!(parse("next 7 days"),  Some(TimeRange::Next { n:7, unit: BucketUnit::Day })); }
    #[test] fn t20_within_30()    { assert_eq!(parse("within 30 days"),Some(TimeRange::Within { n:30, unit: BucketUnit::Day })); }
    #[test] fn t21_in_2025()      {
        assert_eq!(parse("in 2025"), Some(TimeRange::Absolute {
            lo: FilterValue::Date(NaiveDate::from_ymd_opt(2025,1,1).unwrap()),
            hi: FilterValue::Date(NaiveDate::from_ymd_opt(2025,12,31).unwrap()),
        }));
    }
    #[test] fn t22_q1()           {
        assert_eq!(parse("Q1 admissions"), Some(TimeRange::Absolute {
            lo: FilterValue::Date(NaiveDate::from_ymd_opt(2026,1,1).unwrap()),
            hi: FilterValue::Date(NaiveDate::from_ymd_opt(2026,3,31).unwrap()),
        }));
    }
    #[test] fn t23_q3()           {
        assert_eq!(parse("Q3 results"), Some(TimeRange::Absolute {
            lo: FilterValue::Date(NaiveDate::from_ymd_opt(2026,7,1).unwrap()),
            hi: FilterValue::Date(NaiveDate::from_ymd_opt(2026,9,30).unwrap()),
        }));
    }
    #[test] fn t24_month_name()   {
        assert_eq!(parse("march deliveries"), Some(TimeRange::Absolute {
            lo: FilterValue::Date(NaiveDate::from_ymd_opt(2026,3,1).unwrap()),
            hi: FilterValue::Date(NaiveDate::from_ymd_opt(2026,3,31).unwrap()),
        }));
    }
    #[test] fn t25_between_months(){
        assert_eq!(parse("between march and may"), Some(TimeRange::Absolute {
            lo: FilterValue::Date(NaiveDate::from_ymd_opt(2026,3,1).unwrap()),
            hi: FilterValue::Date(NaiveDate::from_ymd_opt(2026,5,31).unwrap()),
        }));
    }
    #[test] fn t26_night_shift()  {
        use chrono::TimeZone;
        let lo = Utc.with_ymd_and_hms(2026,1,14,19,0,0).unwrap();
        let hi = Utc.with_ymd_and_hms(2026,1,15,7,0,0).unwrap();
        assert_eq!(parse("on the night shift"), Some(TimeRange::Absolute {
            lo: FilterValue::Ts(lo),
            hi: FilterValue::Ts(hi),
        }));
    }
    #[test] fn t27_day_shift()    {
        use chrono::TimeZone;
        let lo = Utc.with_ymd_and_hms(2026,1,15,8,0,0).unwrap();
        let hi = Utc.with_ymd_and_hms(2026,1,15,16,0,0).unwrap();
        assert_eq!(parse("day shift missed doses"), Some(TimeRange::Absolute {
            lo: FilterValue::Ts(lo),
            hi: FilterValue::Ts(hi),
        }));
    }
    #[test] fn t28_previous_week(){ assert_eq!(parse("previous week"), Some(TimeRange::LastWeek)); }
    #[test] fn t29_no_time()      { assert_eq!(parse("how many patients"), None); }
    #[test] fn t30_last_12_months(){ assert_eq!(parse("last 12 months"), Some(TimeRange::Last { n:12, unit: BucketUnit::Month })); }
}
