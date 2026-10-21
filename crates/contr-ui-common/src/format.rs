//! Text for sizes, rates, percentages and times, in the units the design uses.

use std::path::Path;
use std::time::{Duration, SystemTime};

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// Decimal size such as `142 MB` or `1.8 GB`: one decimal below ten, none above.
pub fn bytes(value: u64) -> String {
    let (scaled, unit) = scale(value);
    match unit {
        0 => format!("{value} B"),
        _ if scaled < 9.95 => format!("{scaled:.1} {}", UNITS[unit]),
        _ => format!("{scaled:.0} {}", UNITS[unit]),
    }
}

/// Transfer rate such as `41 KB/s`.
pub fn rate(bytes_per_second: f64) -> String {
    let rounded = bytes_per_second.max(0.0).round();
    // Rates stay far below u64::MAX, and the value is non-negative and rounded.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = rounded as u64;
    format!("{}/s", bytes(value))
}

/// `part of total` in the total's unit, such as `87 of 141 MB`.
pub fn bytes_of(part: u64, total: u64) -> String {
    let (_, unit) = scale(total);
    if unit == 0 {
        return format!("{part} of {total} B");
    }
    let divisor = 1000_f64.powi(i32::try_from(unit).unwrap_or(0));
    #[allow(clippy::cast_precision_loss)]
    let (part, total_scaled) = (part as f64 / divisor, total as f64 / divisor);
    if total_scaled < 9.95 {
        format!("{part:.1} of {total_scaled:.1} {}", UNITS[unit])
    } else {
        format!("{part:.0} of {total_scaled:.0} {}", UNITS[unit])
    }
}

/// Percentage with one decimal, such as `3.2%`.
pub fn percent(value: f64) -> String {
    format!("{value:.1}%")
}

/// Time since `then`, such as `14 minutes ago` or `2 weeks ago`.
pub fn ago(then: SystemTime, now: SystemTime) -> String {
    let elapsed = now.duration_since(then).unwrap_or_default();
    if elapsed < Duration::from_secs(60) {
        return "just now".to_owned();
    }
    format!("{} ago", span(elapsed))
}

/// Length of a span in its largest whole unit, such as `2 hours`.
pub fn span(duration: Duration) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;

    let seconds = duration.as_secs();
    let (count, unit) = match seconds {
        s if s < MINUTE => (s, "second"),
        s if s < HOUR => (s / MINUTE, "minute"),
        s if s < DAY => (s / HOUR, "hour"),
        s if s < WEEK => (s / DAY, "day"),
        s if s < MONTH => (s / WEEK, "week"),
        s if s < YEAR => (s / MONTH, "month"),
        s => (s / YEAR, "year"),
    };
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// The first twelve characters of an engine ID, without a `sha256:` prefix.
pub fn short_id(id: &str) -> &str {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    id.get(..12).unwrap_or(id)
}

/// `path` with the user's home directory written as `~`.
pub fn home_relative(path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

fn scale(value: u64) -> (f64, usize) {
    #[allow(clippy::cast_precision_loss)]
    let mut scaled = value as f64;
    let mut unit = 0;
    while scaled >= 999.5 && unit < UNITS.len() - 1 {
        scaled /= 1000.0;
        unit += 1;
    }
    (scaled, unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_decimal_units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(4_100_000), "4.1 MB");
        assert_eq!(bytes(142_300_000), "142 MB");
        assert_eq!(bytes(1_800_000_000), "1.8 GB");
        assert_eq!(bytes(999_700), "1.0 MB");
    }

    #[test]
    fn parts_use_the_total_unit() {
        assert_eq!(bytes_of(87_000_000, 141_000_000), "87 of 141 MB");
        assert_eq!(bytes_of(400_000, 3_600_000), "0.4 of 3.6 MB");
    }

    #[test]
    fn rates_and_percentages() {
        assert_eq!(rate(41_200.0), "41 KB/s");
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(percent(3.24), "3.2%");
    }

    #[test]
    fn spans_pick_the_largest_unit() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
        assert_eq!(ago(now - Duration::from_mins(14), now), "14 minutes ago");
        assert_eq!(ago(now - Duration::from_hours(72), now), "3 days ago");
        assert_eq!(ago(now - Duration::from_hours(360), now), "2 weeks ago");
        assert_eq!(ago(now - Duration::from_secs(10), now), "just now");
        assert_eq!(span(Duration::from_secs(3600)), "1 hour");
    }

    #[test]
    fn ids_and_paths() {
        assert_eq!(short_id("sha256:3f9a1c2b7d04aaaa"), "3f9a1c2b7d04");
        assert_eq!(short_id("abc"), "abc");
        let home = Path::new("/home/minh");
        assert_eq!(home_relative("/home/minh/src/compose.yaml", home), "~/src/compose.yaml");
        assert_eq!(home_relative("/home/minhx/a", home), "/home/minhx/a");
    }
}
