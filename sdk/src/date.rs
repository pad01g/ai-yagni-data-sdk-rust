//! 日付（YYYY-MM-DD）と月（YYYY-MM）。形と範囲（月は 1〜12、日はその月の日数まで）を確かめて読む。
use std::time::{SystemTime, UNIX_EPOCH};

/// 日付。年・月・日の順に比べる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day {
    pub year: u32,
    pub month: u32,
    pub day: u32,
}

fn number(s: &str, len: usize) -> Option<u32> {
    (s.len() == len && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

fn days_in(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// YYYY-MM を（年, 月）として読む。
pub fn month(s: &str) -> Option<(u32, u32)> {
    let (y, m) = s.split_once('-')?;
    let (year, month) = (number(y, 4)?, number(m, 2)?);
    (1..=12).contains(&month).then_some((year, month))
}

/// YYYY-MM-DD を読む。
pub fn day(s: &str) -> Option<Day> {
    let (ym, d) = s.rsplit_once('-')?;
    let (year, month) = month(ym)?;
    let day = number(d, 2)?;
    (1..=days_in(year, month)).contains(&day).then_some(Day { year, month, day })
}

/// 今日の日付（UTC、YYYY-MM-DD）。
pub fn today() -> String {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant の civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_and_days_are_checked_for_range() {
        assert_eq!(month("2026-12"), Some((2026, 12)));
        assert_eq!(month("2026-13"), None);
        assert_eq!(month("2026-00"), None);
        assert_eq!(month("2026-1"), None);
        assert_eq!(month("2027"), None);
        assert_eq!(day("2026-02-28").map(|d| d.day), Some(28));
        assert_eq!(day("2026-02-29"), None);
        assert_eq!(day("2028-02-29").map(|d| d.day), Some(29));
        assert_eq!(day("2100-02-29"), None);
        assert_eq!(day("2026-02-31"), None);
        assert_eq!(day("2026-04-31"), None);
        assert_eq!(day("2026-10-00"), None);
        assert_eq!(day("2026-10-1x"), None);
        assert!(day("2026-10-29") < day("2026-10-30"));
    }

    #[test]
    fn today_is_a_day() {
        let t = today();
        assert!(day(&t).is_some(), "{t}");
        assert!(t.as_str() > "2026-01-01");
    }
}
