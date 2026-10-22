//! Timestamps exactly as JavaScript writes them (`new Date(ms).toISOString()`), without a date library.
use crate::now_ms;

/// Now, as `2026-10-22T05:00:00.000Z`.
pub fn iso_now() -> String { iso_at(now_ms() as i64) }
/// new Date(ms).toISOString()
pub fn iso_at(ms: i64) -> String {
    let s = ms.div_euclid(1000); let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem / 60 % 60, rem % 60, ms.rem_euclid(1000))
}
/// Days since 1970-01-01 → (year, month, day).
pub fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719468; let era = z.div_euclid(146097); let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    #[test]
    fn like_javascript() {
        assert_eq!(super::iso_at(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::iso_at(1792645859919), "2026-10-22T05:10:59.919Z");
        assert_eq!(super::iso_at(951782400000), "2000-02-29T00:00:00.000Z");
    }
}
