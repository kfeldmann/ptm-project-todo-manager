//! Timezone offset cache.
//!
//! [`get_tz_offset_minutes`] returns the local UTC offset in **minutes**
//! (e.g. `+330` for IST, `-300` for EST).  The value is cached for 24 hours;
//! on a cache miss it is obtained by spawning `date +%z`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(24 * 60 * 60);

struct Entry {
    minutes: i64,
    fetched_at: Instant,
}

static CACHE: Mutex<Option<Entry>> = Mutex::new(None);

/// Returns the local timezone offset in minutes east of UTC.
pub fn get_tz_offset_minutes() -> i64 {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ref entry) = *guard {
        if entry.fetched_at.elapsed() < TTL {
            return entry.minutes;
        }
    }
    let minutes = fetch_offset();
    *guard = Some(Entry { minutes, fetched_at: Instant::now() });
    minutes
}

// ─── Internals ────────────────────────────────────────────────────────────────

fn fetch_offset() -> i64 {
    let output = match std::process::Command::new("date").arg("+%z").output() {
        Ok(o) => o,
        Err(e) => {
            crate::log::warn(&format!("fetch_offset: failed to spawn `date +%z`: {e}"));
            return 0;
        }
    };
    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(e) => {
            crate::log::warn(&format!("fetch_offset: `date +%z` output is not valid UTF-8: {e}"));
            return 0;
        }
    };
    match parse_offset(stdout.trim()) {
        Some(minutes) => minutes,
        None => {
            crate::log::warn(&format!("fetch_offset: failed to parse `date +%z` output {:?}", stdout.trim()));
            0
        }
    }
}

/// Parses `date +%z` output (e.g. `+0530`, `-0500`) into signed minutes.
fn parse_offset(s: &str) -> Option<i64> {
    if s.len() < 5 {
        return None;
    }
    let (sign, digits) = match s.chars().next()? {
        '+' => (1i64, &s[1..]),
        '-' => (-1i64, &s[1..]),
        _ => return None,
    };
    let hours: i64 = digits.get(..2)?.parse().ok()?;
    let mins: i64 = digits.get(2..4)?.parse().ok()?;
    Some(sign * (hours * 60 + mins))
}

#[cfg(test)]
mod tests {
    use super::parse_offset;

    #[test]
    fn parses_positive_half_hour() {
        assert_eq!(parse_offset("+0530"), Some(330));
    }

    #[test]
    fn parses_negative() {
        assert_eq!(parse_offset("-0500"), Some(-300));
    }

    #[test]
    fn parses_utc() {
        assert_eq!(parse_offset("+0000"), Some(0));
    }

    #[test]
    fn rejects_short_input() {
        assert_eq!(parse_offset("+05"), None);
    }
}
