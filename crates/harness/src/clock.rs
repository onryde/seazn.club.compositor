//! Wall clock text, both ways the harness meets it: MediaMTX's segment names
//! (`recordPath: /rec/%path/%Y-%m-%d_%H-%M-%S-%f`, UTC in the harness's container) and the
//! compositor's log stamps (`ts=<RFC 3339, ms, UTC>`, `compositor::logged`). All parsing is by hand,
//! so the checker reads no clock and no time-zone database.
use std::time::{SystemTime, UNIX_EPOCH};

/// The host's wall clock, in epoch milliseconds (the camera's `emit.jsonl`, fake-control's
/// `received_ms`). Every harness container shares the host's kernel clock.
pub fn now_ms() -> i64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// The same clock in epoch microseconds: Owed 81's probes (`wire.jsonl`, `tap.jsonl`) time frames a
/// millisecond apart, finer than `now_ms`.
pub fn now_us() -> i64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn num(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn epoch_us(date: [&str; 3], clock: [&str; 3], micros: u64) -> Option<i64> {
    let (y, mo, d) = (num(date[0])?, num(date[1])?, num(date[2])?);
    let (h, mi, s) = (num(clock[0])?, num(clock[1])?, num(clock[2])?);
    if date[0].len() != 4 || !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60
    {
        return None;
    }
    let days = days_from_civil(i64::from(y), mo, d);
    let secs = days * 86_400 + i64::from(h) * 3_600 + i64::from(mi) * 60 + i64::from(s);
    Some(secs * 1_000_000 + i64::try_from(micros).ok()?)
}

/// A segment's start, in epoch microseconds, from its file name (`2026-10-23_12-00-00-123456.mp4`;
/// the extension is optional). `None` for any other name: the checker refuses an unknown file
/// rather than guess its time.
pub fn segment_start_us(file_name: &str) -> Option<i64> {
    let stem = file_name.strip_suffix(".mp4").unwrap_or(file_name);
    let (date, rest) = stem.split_once('_')?;
    let date: Vec<&str> = date.split('-').collect();
    let rest: Vec<&str> = rest.split('-').collect();
    if date.len() != 3 || rest.len() != 4 || rest[3].len() != 6 {
        return None;
    }
    let micros = u64::from(num(rest[3])?);
    epoch_us([date[0], date[1], date[2]], [rest[0], rest[1], rest[2]], micros)
}

/// The epoch milliseconds of a compositor log line's `ts=2026-10-23T12:00:00.123Z`, or `None`.
pub fn log_ts_ms(line: &str) -> Option<i64> {
    let ts = line.strip_prefix("ts=")?.split(' ').next()?;
    let ts = ts.strip_suffix('Z')?;
    let (date, clock) = ts.split_once('T')?;
    let date: Vec<&str> = date.split('-').collect();
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, "0"));
    let hms: Vec<&str> = hms.split(':').collect();
    if date.len() != 3 || hms.len() != 3 || frac.is_empty() || frac.len() > 9 {
        return None;
    }
    // The fraction as microseconds: "123" is 123 000 us.
    let mut padded = frac.to_owned();
    while padded.len() < 6 {
        padded.push('0');
    }
    let micros = u64::from(num(&padded[..6])?);
    epoch_us([date[0], date[1], date[2]], [hms[0], hms[1], hms[2]], micros).map(|us| us.div_euclid(1_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected epochs are written out from the calendar, not computed by the code under test:
    // 2026-10-23T00:00:00Z is day 20 749 after the epoch (56 years incl. 14 leap days to 2026-01-01
    // = 20 454 days, plus 295 days to 23 October).
    const OCT_23_2026_S: i64 = 20_749 * 86_400;

    #[test]
    fn now_us_reads_the_same_clock_as_now_ms() {
        let before = now_ms();
        let us = now_us();
        let after = now_ms();
        assert!(
            before <= us / 1_000 && us / 1_000 <= after,
            "{before} <= {us} us <= {after}"
        );
    }

    #[test]
    fn segment_names_read_as_utc_microseconds() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 10, 23), 20_749);
        let us = segment_start_us("2026-10-23_12-34-56-789012.mp4").expect("parses");
        assert_eq!(
            us,
            (OCT_23_2026_S + 12 * 3_600 + 34 * 60 + 56) * 1_000_000 + 789_012
        );
        assert_eq!(
            segment_start_us("2026-10-23_12-34-56-789012"),
            Some(us),
            "the extension is optional"
        );
    }

    #[test]
    fn other_names_are_refused() {
        for bad in [
            "",
            "rec.mp4",
            "2026-10-23_12-34-56.mp4",
            "2026-10-23_12-34-56-78901.mp4",
            "2026-13-23_12-34-56-789012.mp4",
            "2026-10-23_24-34-56-789012.mp4",
            "2026-10-23_12-34-5x-789012.mp4",
            "26-10-23_12-34-56-789012.mp4",
        ] {
            assert_eq!(segment_start_us(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn log_stamps_read_as_epoch_ms() {
        let line = "ts=2026-10-23T12:00:00.123Z level=info msg=cef crashed gen=2";
        assert_eq!(log_ts_ms(line), Some((OCT_23_2026_S + 12 * 3_600) * 1_000 + 123));
        assert_eq!(
            log_ts_ms("ts=2026-10-23T12:00:00Z level=info msg=x"),
            Some((OCT_23_2026_S + 43_200) * 1_000)
        );
        assert_eq!(log_ts_ms("level=info msg=no stamp"), None);
        assert_eq!(
            log_ts_ms("ts=2026-10-23T12:00:00.123 level=info"),
            None,
            "not UTC"
        );
    }
}
