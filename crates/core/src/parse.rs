//! Parsing what people type into a cell (and numeric text in formulas).
//!
//! Recognises numbers with thousands separators, percentages, currency, scientific notation,
//! fractions (`1 1/2`), dates (`3/4/2026`, `2026-03-04`, `4-Mar-2026`, `Mar 4, 2026`), times
//! (`10:30`, `10:30:15 PM`) and date-times. Locale: en-US.

use crate::date::{DateSystem, MONTHS, serial_from_ymd, time_fraction};
use crate::value::{CellError, Value};

/// A parsed entry: its value and the number format Excel would apply automatically.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub value: Value,
    pub format: Option<&'static str>,
}

/// Interprets typed cell input (not formulas). Text that isn't a number, date, boolean or error
/// stays text. A leading apostrophe forces text.
pub fn parse_input(s: &str, sys: DateSystem) -> Parsed {
    if s.is_empty() {
        return Parsed { value: Value::Empty, format: None };
    }
    if let Some(rest) = s.strip_prefix('\'') {
        return Parsed { value: Value::text(rest), format: None };
    }
    let t = s.trim();
    if t.eq_ignore_ascii_case("TRUE") {
        return Parsed { value: Value::Bool(true), format: None };
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return Parsed { value: Value::Bool(false), format: None };
    }
    if let Some(e) = CellError::parse(t) {
        return Parsed { value: Value::Error(e), format: None };
    }
    if let Some((n, f)) = parse_numeric(t, sys) {
        return Parsed { value: Value::number(n), format: f };
    }
    Parsed { value: Value::text(s), format: None }
}

/// Numeric text as arithmetic coerces it ("1,000", "50%", "$3", "1/2/2020", "10:00").
pub fn parse_number_text(s: &str) -> Option<f64> {
    parse_number_text_in(s, DateSystem::D1900)
}

/// Like [`parse_number_text`], with date text read in the given workbook date system.
pub fn parse_number_text_in(s: &str, sys: DateSystem) -> Option<f64> {
    parse_numeric(s.trim(), sys).map(|(n, _)| n)
}

fn parse_numeric(t: &str, sys: DateSystem) -> Option<(f64, Option<&'static str>)> {
    if t.is_empty() {
        return None;
    }
    // Parenthesised negatives: (100) = -100.
    if let Some(inner) = t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        return parse_numeric(inner.trim(), sys).map(|(n, f)| (-n, f));
    }
    if let Some(r) = parse_plain(t) {
        return Some(r);
    }
    if let Some(r) = parse_fraction(t) {
        return Some((r, Some("# ?/?")));
    }
    if let Some(r) = parse_datetime(t, sys) {
        return Some(r);
    }
    None
}

/// Signs, currency, thousands separators, percent, exponent.
fn parse_plain(t: &str) -> Option<(f64, Option<&'static str>)> {
    let mut s = t;
    let mut neg = false;
    let mut currency = false;
    let mut percent = false;
    if let Some(r) = s.strip_prefix('-') {
        neg = true;
        s = r.trim_start();
    } else if let Some(r) = s.strip_prefix('+') {
        s = r.trim_start();
    }
    if let Some(r) = s.strip_prefix('$') {
        currency = true;
        s = r.trim_start();
        if !neg && let Some(r2) = s.strip_prefix('-') {
            neg = true;
            s = r2;
        }
    }
    if let Some(r) = s.strip_suffix('%') {
        percent = true;
        s = r.trim_end();
    }
    if s.is_empty() {
        return None;
    }
    let has_sep = s.contains(',');
    let digits: String = if has_sep {
        // Thousands separators must be in groups of three before the decimal point.
        let (int, frac) = s.split_once('.').map_or((s, None), |(a, b)| (a, Some(b)));
        let groups: Vec<&str> = int.split(',').collect();
        let first = groups.first()?;
        if first.is_empty() || first.len() > 3 || groups.iter().skip(1).any(|g| g.len() != 3) {
            return None;
        }
        let mut d = groups.concat();
        if let Some(f) = frac {
            d.push('.');
            d.push_str(f);
        }
        d
    } else {
        s.to_string()
    };
    let b = digits.as_bytes();
    if !b.iter().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')) {
        return None;
    }
    if !b.first().is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
        return None;
    }
    let mut n: f64 = digits.parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    let sci = digits.contains(['e', 'E']);
    if percent {
        n /= 100.0;
    }
    if neg {
        n = -n;
    }
    let fmt = if currency {
        Some(if digits.contains('.') { "\"$\"#,##0.00" } else { "\"$\"#,##0" })
    } else if percent {
        Some(if digits.contains('.') { "0.00%" } else { "0%" })
    } else if sci {
        Some("0.00E+00")
    } else if has_sep {
        Some(if digits.contains('.') { "#,##0.00" } else { "#,##0" })
    } else {
        None
    };
    Some((n, fmt))
}

/// `1/2` (only with a whole part, else it's a date), `1 1/2`, `0 3/4`.
fn parse_fraction(t: &str) -> Option<f64> {
    let (whole, frac) = t.split_once(' ')?;
    let neg = whole.starts_with('-');
    let w: f64 = whole.trim_start_matches('-').parse().ok()?;
    let (a, b) = frac.trim().split_once('/')?;
    let a: f64 = a.parse().ok()?;
    let b: f64 = b.parse().ok()?;
    if b == 0.0 || !w.is_finite() {
        return None;
    }
    let v = w + a / b;
    Some(if neg { -v } else { v })
}

fn month_from_name(s: &str) -> Option<u32> {
    let l = s.to_ascii_lowercase();
    if l.len() < 3 {
        return None;
    }
    MONTHS.iter().position(|m| m.to_ascii_lowercase().starts_with(&l)).map(|i| i as u32 + 1)
}

/// Time part: `h:mm`, `h:mm:ss`, `h:mm:ss.000`, `h AM`, with optional AM/PM.
fn parse_time(t: &str) -> Option<(f64, bool, bool)> {
    let t = t.trim();
    let lower = t.to_ascii_lowercase();
    let (body, ampm) = if let Some(b) = lower.strip_suffix("am").or_else(|| lower.strip_suffix('a')) {
        (b.trim().to_string(), Some(false))
    } else if let Some(b) = lower.strip_suffix("pm").or_else(|| lower.strip_suffix('p')) {
        (b.trim().to_string(), Some(true))
    } else {
        (lower.clone(), None)
    };
    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() == 1 && ampm.is_none() {
        return None;
    }
    if parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut h: f64 = parts.first()?.parse().ok()?;
    let m: f64 = parts.get(1).map_or(Some(0.0), |p| p.parse().ok())?;
    let s: f64 = parts.get(2).map_or(Some(0.0), |p| p.parse().ok())?;
    if m >= 60.0 || s >= 60.0 || h < 0.0 || m < 0.0 || s < 0.0 {
        return None;
    }
    if let Some(pm) = ampm {
        if !(0.0..=12.0).contains(&h) {
            return None;
        }
        if h == 12.0 {
            h = 0.0;
        }
        if pm {
            h += 12.0;
        }
    } else if h >= 10000.0 {
        return None;
    }
    Some((time_fraction(h, m, s), parts.len() == 3, ampm.is_some()))
}

fn parse_date(t: &str, sys: DateSystem) -> Option<(f64, &'static str)> {
    let t = t.trim().trim_end_matches(',');
    let num = |s: &str| s.trim().parse::<i64>().ok();
    // ISO yyyy-mm-dd or yyyy/mm/dd
    let seps: &[char] = &['/', '-', '.'];
    let parts: Vec<&str> = t.split(seps).collect();
    if parts.len() == 3 {
        let (a, b, c) = (parts[0].trim(), parts[1].trim(), parts[2].trim());
        if a.len() == 4
            && let (Some(y), Some(m), Some(d)) = (num(a), num(b), num(c))
        {
            return valid_ymd(sys, y, m, d).map(|s| (s, "yyyy-mm-dd"));
        }
        if let (Some(m), Some(d), Some(y)) = (num(a), num(b), num(c)) {
            let y = if c.len() <= 2 { if y < 30 { 2000 + y } else { 1900 + y } } else { y };
            return valid_ymd(sys, y, m, d).map(|s| (s, "m/d/yyyy"));
        }
        // 4-Mar-2026
        if let (Some(d), Some(m), Some(y)) = (num(a), month_from_name(b), num(c)) {
            let y = if c.len() <= 2 { if y < 30 { 2000 + y } else { 1900 + y } } else { y };
            return valid_ymd(sys, y, m as i64, d).map(|s| (s, "d-mmm-yy"));
        }
    }
    if parts.len() == 2 {
        let (a, b) = (parts[0].trim(), parts[1].trim());
        // m/d in the current year is ambiguous without a clock; use month/day of 2026? Excel uses
        // the current year. We accept d-mmm / mmm-d only with month names, and m/d → current year.
        if let (Some(m), Some(d)) = (num(a), num(b)) {
            let y = current_year();
            if (1..=12).contains(&m) && (1..=31).contains(&d) {
                return valid_ymd(sys, y, m, d).map(|s| (s, "d-mmm"));
            }
            // m/yyyy
            if (1..=12).contains(&m) && b.len() == 4 {
                return valid_ymd(sys, d, m, 1).map(|s| (s, "mmm-yy"));
            }
        }
        if let (Some(d), Some(m)) = (num(a), month_from_name(b)) {
            return valid_ymd(sys, current_year(), m as i64, d).map(|s| (s, "d-mmm"));
        }
        if let (Some(m), Some(y)) = (month_from_name(a), num(b)) {
            let y = if b.len() <= 2 { if y < 30 { 2000 + y } else { 1900 + y } } else { y };
            return valid_ymd(sys, y, m as i64, 1).map(|s| (s, "mmm-yy"));
        }
    }
    // "Mar 4, 2026" / "March 4 2026" / "4 March 2026"
    let words: Vec<&str> = t.split([' ', ',']).filter(|w| !w.is_empty()).collect();
    if words.len() == 3 {
        // Two-digit years follow the same 2029 rule as the dashed forms above.
        let expand = |w: &str, y: i64| if w.len() <= 2 { if y < 30 { 2000 + y } else { 1900 + y } } else { y };
        if let (Some(m), Some(d), Some(y)) = (month_from_name(words[0]), num(words[1]), num(words[2])) {
            let y = expand(words[2], y);
            return valid_ymd(sys, y, m as i64, d).map(|s| (s, "d-mmm-yy"));
        }
        if let (Some(d), Some(m), Some(y)) = (num(words[0]), month_from_name(words[1]), num(words[2])) {
            let y = expand(words[2], y);
            return valid_ymd(sys, y, m as i64, d).map(|s| (s, "d-mmm-yy"));
        }
    }
    None
}

/// A date typed into a cell formatted as a date, read in that format's order (`day_first` for
/// "dd/mm/yy") and with the short forms that save typing: "16/10/26", "16.10.2026", "16-10-26",
/// "16 10 26", "161026", "16102026"; without the year ("16/10", "1610") or with the day alone
/// ("16"), the most recent such day up to today, since what is typed is rarely today but often
/// a past day. A four-digit year first ("2026-10-16") is read as such. `None` when the text isn't
/// such a date: it then goes through [`parse_input`].
pub fn parse_date_entry(t: &str, sys: DateSystem, day_first: bool) -> Option<f64> {
    let today = match crate::date::unix_now_ms() {
        Some(ms) => {
            let (y, m, d) = crate::date::civil_from_days((ms / 86_400_000.0).floor() as i64);
            (i64::from(y), i64::from(m), i64::from(d))
        }
        None => (current_year(), 12, 31),
    };
    parse_date_entry_on(t, sys, day_first, today)
}

/// [`parse_date_entry`] with today given as (year, month, day).
pub fn parse_date_entry_on(t: &str, sys: DateSystem, day_first: bool, today: (i64, i64, i64)) -> Option<f64> {
    let (year, month, _) = today;
    let now = serial_from_ymd(sys, today.0, today.1, today.2)?;
    let t = t.trim();
    let two_digit_year = |y: i64| if y < 30 { 2000 + y } else { 1900 + y };
    let day_month = |a: i64, b: i64| if day_first { (a, b) } else { (b, a) };
    let num = |s: &str| if !s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit()) { s.parse::<i64>().ok() } else { None };
    // Day and month without a year: this year, or last year if that is still to come.
    let latest_year = |d: i64, m: i64| valid_ymd(sys, year, m, d).filter(|&n| n <= now).or_else(|| valid_ymd(sys, year - 1, m, d));
    // The day alone: this month, or the latest month before that has that day and is past.
    let latest_month = |d: i64| {
        (0..3).find_map(|back| {
            let k = year * 12 + (month - 1) - back;
            valid_ymd(sys, k.div_euclid(12), k.rem_euclid(12) + 1, d).filter(|&n| n <= now)
        })
    };
    if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
        let at = |a: usize, b: usize| t.get(a..b).and_then(|s| s.parse::<i64>().ok());
        return match t.len() {
            1 | 2 => latest_month(at(0, t.len())?),
            4 => {
                let (d, m) = day_month(at(0, 2)?, at(2, 4)?);
                latest_year(d, m)
            }
            6 => {
                let (d, m) = day_month(at(0, 2)?, at(2, 4)?);
                valid_ymd(sys, two_digit_year(at(4, 6)?), m, d)
            }
            8 => {
                let (d, m) = day_month(at(0, 2)?, at(2, 4)?);
                valid_ymd(sys, at(4, 8)?, m, d)
            }
            _ => None,
        };
    }
    let parts: Vec<&str> = t.split(['/', '.', '-', ' ']).filter(|p| !p.is_empty()).collect();
    let n: Vec<i64> = parts.iter().map(|p| num(p)).collect::<Option<_>>()?;
    match (n.as_slice(), parts.first().map(|p| p.len())) {
        ([a, b], _) => {
            let (d, m) = day_month(*a, *b);
            latest_year(d, m)
        }
        ([y, m, d], Some(4)) => valid_ymd(sys, *y, *m, *d),
        ([a, b, y], _) => {
            let (d, m) = day_month(*a, *b);
            let y = if parts.get(2).is_some_and(|p| p.len() <= 2) { two_digit_year(*y) } else { *y };
            valid_ymd(sys, y, m, d)
        }
        _ => None,
    }
}

fn valid_ymd(sys: DateSystem, y: i64, m: i64, d: i64) -> Option<f64> {
    // Dates before 1900 stay text (`DATE()` would add 1900 to such a year).
    if !(1..=12).contains(&m) || d < 1 || y < 1900 {
        return None;
    }
    let max = crate::date::days_in_month_in(sys, y as i32, m as u32) as i64;
    if d > max {
        return None;
    }
    serial_from_ymd(sys, y, m, d)
}

/// The year used for dates typed without one, from the system clock (browser clock on wasm).
/// Falls back to 2026 only when no clock is available.
pub fn current_year() -> i64 {
    if let Some(ms) = crate::date::unix_now_ms() {
        let days = (ms / 86_400_000.0).floor() as i64;
        return crate::date::civil_from_days(days).0 as i64;
    }
    2026
}

fn parse_datetime(t: &str, sys: DateSystem) -> Option<(f64, Option<&'static str>)> {
    if let Some((tf, secs, ampm)) = parse_time(t) {
        let f = match (secs, ampm) {
            // 24 hours or more: elapsed hours, so 25:00 shows as 25:00:00, not 1:00.
            _ if tf >= 1.0 => "[h]:mm:ss",
            (false, false) => "h:mm",
            (true, false) => "h:mm:ss",
            (false, true) => "h:mm AM/PM",
            (true, true) => "h:mm:ss AM/PM",
        };
        return Some((tf, Some(f)));
    }
    if let Some((d, f)) = parse_date(t, sys) {
        return Some((d, Some(f)));
    }
    // date + time: split at the last space(s) before a time-looking tail.
    for (i, _) in t.match_indices(' ') {
        let (a, b) = (t.get(..i)?, t.get(i + 1..)?);
        if let (Some((d, _)), Some((tf, _, _))) = (parse_date(a, sys), parse_time(b)) {
            return Some((d + tf, Some("m/d/yyyy h:mm")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Typed in a "dd/mm/yy" cell on 16 October 2026.
    fn entry(t: &str) -> Option<f64> {
        parse_date_entry_on(t, DateSystem::D1900, true, (2026, 10, 16))
    }

    fn ymd(y: i64, m: i64, d: i64) -> Option<f64> {
        serial_from_ymd(DateSystem::D1900, y, m, d)
    }

    #[test]
    fn date_entries_in_a_day_first_cell() {
        let oct16 = serial_from_ymd(DateSystem::D1900, 2026, 10, 16);
        for t in ["16/10/26", "16/10/2026", "16.10.26", "16-10-26", "16 10 26", " 16/10/26 ", "16/10", "16.10", "1610", "161026", "16102026", "16", "2026-10-16"] {
            assert_eq!(entry(t), oct16, "{t:?}");
        }
        // Day first, whatever the day: 5 October, not 10 May.
        assert_eq!(entry("05/10/26"), ymd(2026, 10, 5));
        assert_eq!(entry("5/1"), ymd(2026, 1, 5));
        assert_eq!(entry("010199"), ymd(1999, 1, 1));
        // Without a year or a month: the latest such day, today included, never one to come.
        assert_eq!(entry("20/12"), ymd(2025, 12, 20));
        assert_eq!(entry("2012"), ymd(2025, 12, 20));
        assert_eq!(entry("20"), ymd(2026, 9, 20));
        assert_eq!(entry("31"), ymd(2026, 8, 31), "September has no 31st");
        assert_eq!(entry("1"), ymd(2026, 10, 1));
        assert_eq!(entry("29/02"), None, "no 29 February this year or last");
        assert_eq!(parse_date_entry_on("15", DateSystem::D1900, true, (2026, 1, 10)), ymd(2025, 12, 15), "across the new year");
        for t in ["32/10/26", "16/13", "31/09/26", "123", "12345", "abc", "16/10/26/1", "", "16:30", "1.5.2.6"] {
            assert_eq!(entry(t), None, "{t:?}");
        }
    }

    #[test]
    fn date_entries_in_a_month_first_cell() {
        let oct16 = serial_from_ymd(DateSystem::D1900, 2026, 10, 16);
        for t in ["10/16/26", "10/16", "1016", "101626", "16"] {
            assert_eq!(parse_date_entry_on(t, DateSystem::D1900, false, (2026, 10, 16)), oct16, "{t:?}");
        }
    }

    fn p(s: &str) -> Parsed {
        parse_input(s, DateSystem::D1900)
    }

    #[test]
    fn month_name_short_year() {
        for s in ["Mar 4, 26", "Mar 4 26", "March 4 26", "4 March 26", "Mar 4, 2026"] {
            assert_eq!(p(s).value, Value::Number(46085.0), "{s}");
        }
        assert_eq!(p("Mar 4, 99").value, p("Mar 4, 1999").value);
        assert_eq!(p("Mar 4, 1776").value, Value::text("Mar 4, 1776"));
        assert_eq!(p("Feb 30, 26").value, Value::text("Feb 30, 26"));
    }

    #[test]
    fn numbers() {
        assert_eq!(p("42").value, Value::Number(42.0));
        assert_eq!(p("-3.5").value, Value::Number(-3.5));
        assert_eq!(p("1,234").format, Some("#,##0"));
        assert_eq!(p("1,234").value, Value::Number(1234.0));
        assert_eq!(p("1,23").value, Value::text("1,23"));
        assert_eq!(p("12%").value, Value::Number(0.12));
        assert_eq!(p("$1,200.50").value, Value::Number(1200.5));
        assert_eq!(p("$1,200.50").format, Some("\"$\"#,##0.00"));
        assert_eq!(p("(100)").value, Value::Number(-100.0));
        assert_eq!(p("1e3").value, Value::Number(1000.0));
        assert_eq!(p("1 1/2").value, Value::Number(1.5));
        assert_eq!(p(".5").value, Value::Number(0.5));
    }

    #[test]
    fn texts_and_specials() {
        assert_eq!(p("hello").value, Value::text("hello"));
        assert_eq!(p("'123").value, Value::text("123"));
        assert_eq!(p("true").value, Value::Bool(true));
        assert_eq!(p("#N/A").value, Value::Error(CellError::NA));
        assert_eq!(p("e").value, Value::text("e"));
        assert_eq!(p("-").value, Value::text("-"));
        assert_eq!(p("1.2.3.4").value, Value::text("1.2.3.4"));
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(p("10/7/2026").value, Value::Number(46302.0));
        assert_eq!(p("2026-10-07").value, Value::Number(46302.0));
        assert_eq!(p("7-Oct-2026").value, Value::Number(46302.0));
        assert_eq!(p("Oct 7, 2026").value, Value::Number(46302.0));
        assert_eq!(p("2/30/2026").value, Value::text("2/30/2026"));
        assert_eq!(p("7/4/1776").value, Value::text("7/4/1776"));
        assert_eq!(p("1776-07-04").value, Value::text("1776-07-04"));
        assert_eq!(p("4-Jul-1776").value, Value::text("4-Jul-1776"));
        assert_eq!(p("12/31/1899").value, Value::text("12/31/1899"));
        assert_eq!(p("1/1/1900").value, Value::Number(1.0));
        assert_eq!(p("12:00").value, Value::Number(0.5));
        assert_eq!(p("6:00 PM").value, Value::Number(0.75));
        assert_eq!(p("12:00 AM").value, Value::Number(0.0));
        assert_eq!(p("10/7/2026 18:00").value, Value::Number(46302.75));
        assert_eq!(p("25:00").value, Value::Number(25.0 / 24.0));
        assert_eq!(p("25:00").format, Some("[h]:mm:ss"));
        assert_eq!(p("23:59").format, Some("h:mm"));
    }

    #[test]
    fn fictitious_1900_02_29() {
        // Excel accepts 29 February 1900 (serial 60) and keeps 1 March 1900 at 61.
        assert_eq!(p("2/29/1900").value, Value::Number(60.0));
        assert_eq!(p("1900-02-29").value, Value::Number(60.0));
        assert_eq!(p("29-Feb-1900").value, Value::Number(60.0));
        assert_eq!(p("2/28/1900").value, Value::Number(59.0));
        assert_eq!(p("3/1/1900").value, Value::Number(61.0));
        assert_eq!(p("2/29/1901").value, Value::text("2/29/1901"));
        assert_eq!(p("2/30/1900").value, Value::text("2/30/1900"));
        // The 1904 system starts after it.
        assert_eq!(parse_input("2/29/1900", DateSystem::D1904).value, Value::text("2/29/1900"));
        assert_eq!(parse_input("2/29/1904", DateSystem::D1904).value, Value::Number(59.0));
    }
}
