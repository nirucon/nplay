//! Local-time helpers matching the 1.x `datetime` arithmetic (fixed UTC
//! offset of the instant, like `datetime.fromtimestamp(t).astimezone()`),
//! UTC ISO-8601 formatting and uuid4.

/// `(year, month, day, weekday Monday=0, utc offset seconds)` of `ts` in local time.
pub fn local_parts(ts: f64) -> (i64, i64, i64, i64, i64) {
    let t = ts.floor() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        let days = (ts / 86400.0).floor() as i64;
        let (y, m, d) = civil_from_days(days);
        return (y, m, d, (days + 3).rem_euclid(7), 0);
    }
    (
        tm.tm_year as i64 + 1900,
        tm.tm_mon as i64 + 1,
        tm.tm_mday as i64,
        (tm.tm_wday as i64 + 6) % 7,
        tm.tm_gmtoff as i64,
    )
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Local midnight that ends the day containing `ts` (1.x tracker split).
pub fn next_local_midnight(ts: f64) -> f64 {
    let (y, m, d, _, off) = local_parts(ts);
    ((days_from_civil(y, m, d) + 1) * 86400 - off) as f64
}

/// Start of the `day|week|month|year` period containing `now`; `None` for
/// `all`; `None` for an unknown period.
pub fn period_start(period: &str, now: f64) -> Option<Option<f64>> {
    let (y, m, d, wd, off) = local_parts(now);
    let days = match period {
        "day" => days_from_civil(y, m, d),
        "week" => days_from_civil(y, m, d) - wd,
        "month" => days_from_civil(y, m, 1),
        "year" => days_from_civil(y, 1, 1),
        "all" => return Some(None),
        _ => return None,
    };
    Some(Some((days * 86400 - off) as f64))
}

/// `datetime.fromtimestamp(ts, timezone.utc).isoformat().replace('+00:00','Z')`.
pub fn iso_utc(ts: f64) -> String {
    let mut secs = ts.floor() as i64;
    let mut us = ((ts - ts.floor()) * 1e6).round() as i64;
    if us >= 1_000_000 {
        secs += 1;
        us -= 1_000_000;
    }
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, mo, d) = civil_from_days(days);
    let base = format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    );
    if us != 0 {
        format!("{base}.{us:06}Z")
    } else {
        format!("{base}Z")
    }
}

/// Random UUID version 4 in the canonical lowercase form.
pub fn uuid4() -> String {
    let mut b = [0u8; 16];
    for x in b.iter_mut() {
        *x = fastrand::u8(..);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Python `str(float)` for finite values (`1.0`, `1763800000.25`).
pub fn py_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_and_calendar() {
        assert_eq!(iso_utc(1000.0), "1970-01-01T00:16:40Z");
        assert_eq!(iso_utc(1000.5), "1970-01-01T00:16:40.500000Z");
        assert_eq!(iso_utc(1_791_590_398.0), "2026-10-09T23:59:58Z");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        let u = uuid4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_eq!(py_float(5.0), "5.0");
        assert_eq!(py_float(0.25), "0.25");
        let now = 1_791_590_398.0;
        let mid = next_local_midnight(now);
        assert!(mid > now && mid - now <= 86400.0 + 3600.0);
        assert!(period_start("bogus", now).is_none());
        assert_eq!(period_start("all", now), Some(None));
        let w = period_start("week", now).unwrap().unwrap();
        let dd = period_start("day", now).unwrap().unwrap();
        assert!(w <= dd && dd <= now);
        assert_eq!(local_parts(w).3, 0);
    }
}
