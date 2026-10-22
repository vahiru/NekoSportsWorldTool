//! 跑步开始时间计算（纯函数，CLI 使用）。
//!
//! 规则：开始 + 用时 + 抖动余量 ≤ 当前时刻（flow.rs 提交时会给开始时间再加 0-4s
//! 抖动），超出时把开始时刻提前到上限并标记 `clamped`；非法输入返回错误，
//! 不再静默回退到当前时刻。GUI（ui/run.rs）目前有同规则的私有实现，可后续切换到本模块。

use chrono::{DateTime, Duration, TimeZone};

/// flow.rs 提交时给开始时间加 0-4s 抖动，预留 5s 余量。
pub const JITTER_MARGIN_MS: i64 = 5_000;
/// 最多往前 3 天（与 GUI 日期下拉一致）。
pub const MAX_DAYS_AGO: i64 = 3;
/// `--ago` 上限（分钟）：与 3 天上限一致。
pub const MAX_AGO_MIN: i64 = (MAX_DAYS_AGO + 1) * 24 * 60;
/// 默认随机窗口：30-300 分钟前。
pub const DEFAULT_AGO_MIN: i64 = 30;
pub const DEFAULT_AGO_MAX: i64 = 300;

/// 计算结果：`clamped` 为 true 表示原时刻会让结束时刻越过当前时间，已提前。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartTime {
    pub ms: i64,
    pub clamped: bool,
}

/// 开跑时刻上限：当前时刻 - 用时 - 抖动余量。
pub fn latest_start_ms(now_ms: i64, dur_s: i64) -> i64 {
    now_ms - dur_s.max(0) * 1000 - JITTER_MARGIN_MS
}

/// 把开始时刻钳制到上限以内。
pub fn clamp(start_ms: i64, now_ms: i64, dur_s: i64) -> StartTime {
    let latest = latest_start_ms(now_ms, dur_s);
    if start_ms > latest {
        StartTime { ms: latest, clamped: true }
    } else {
        StartTime { ms: start_ms, clamped: false }
    }
}

/// 解析 `HH:MM`（也接受仅小时 `H`，分钟为 0）。时 0-23、分 0-59，越界或非数字报错。
pub fn parse_hhmm(s: &str) -> Result<(u32, u32), String> {
    let s = s.trim();
    let (h, m) = s.split_once(':').unwrap_or((s, "0"));
    let num = |part: &str, max: u32| -> Option<u32> {
        if part.is_empty() || part.len() > 2 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u32>().ok().filter(|v| *v <= max)
    };
    match (num(h, 23), num(m, 59)) {
        (Some(h), Some(m)) => Ok((h, m)),
        _ => Err(format!("时间格式应为 HH:MM（00:00-23:59），收到 {s:?}")),
    }
}

/// 校验天数偏移（0=今天，最多 3 天前）。
pub fn check_days_ago(days_ago: i64) -> Result<i64, String> {
    if (0..=MAX_DAYS_AGO).contains(&days_ago) {
        Ok(days_ago)
    } else {
        Err(format!("天数应为 0-{MAX_DAYS_AGO}，收到 {days_ago}"))
    }
}

/// 指定时刻：`days_ago` 天前（0=今天）的 `hour:minute`，再按用时钳制。
/// 本地时区不存在的时刻（夏令时跳变）报错；重复时刻取较早的一个。
pub fn specified<Tz: TimeZone>(
    now: &DateTime<Tz>,
    days_ago: i64,
    hour: u32,
    minute: u32,
    dur_s: i64,
) -> Result<StartTime, String> {
    let days_ago = check_days_ago(days_ago)?;
    let date = now.date_naive() - Duration::days(days_ago);
    let naive = date
        .and_hms_opt(hour, minute, 0)
        .ok_or_else(|| format!("无效时刻 {hour:02}:{minute:02}"))?;
    let t = now
        .timezone()
        .from_local_datetime(&naive)
        .earliest()
        .ok_or_else(|| format!("{naive} 在本地时区不存在（夏令时切换），请换一个时刻"))?;
    Ok(clamp(t.timestamp_millis(), now.timestamp_millis(), dur_s))
}

/// `--ago N`：N 分钟前开跑（1 ≤ N ≤ MAX_AGO_MIN），再按用时钳制。
pub fn ago(now_ms: i64, ago_min: i64, dur_s: i64) -> Result<StartTime, String> {
    if !(1..=MAX_AGO_MIN).contains(&ago_min) {
        return Err(format!("分钟数应为 1-{MAX_AGO_MIN}，收到 {ago_min}"));
    }
    Ok(clamp(now_ms - ago_min * 60_000, now_ms, dur_s))
}

/// 默认：30-300 分钟前随机（`r` ∈ [0,1) 为随机数），再按用时钳制。
pub fn random_ago(now_ms: i64, dur_s: i64, r: f64) -> StartTime {
    let r = if r.is_finite() { r.clamp(0.0, 1.0) } else { 0.0 };
    let span_ms = ((DEFAULT_AGO_MAX - DEFAULT_AGO_MIN) * 60_000) as f64;
    let start = now_ms - DEFAULT_AGO_MIN * 60_000 - (r * span_ms) as i64;
    clamp(start, now_ms, dur_s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    fn at(h: u32, m: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 22, h, m, 0)
            .unwrap()
    }

    fn ends_by_now(t: StartTime, now_ms: i64, dur_s: i64) -> bool {
        t.ms + dur_s * 1000 + JITTER_MARGIN_MS <= now_ms
    }

    #[test]
    fn hhmm_parsing() {
        assert_eq!(parse_hhmm("07:30"), Ok((7, 30)));
        assert_eq!(parse_hhmm(" 7:05 "), Ok((7, 5)));
        assert_eq!(parse_hhmm("23:59"), Ok((23, 59)));
        assert_eq!(parse_hhmm("8"), Ok((8, 0)));
        for bad in ["24:00", "7:60", "7:xx", "ab", "", ":", "7:", "-1:00", "07:300", "7:5:0"] {
            assert!(parse_hhmm(bad).is_err(), "{bad} 应报错");
        }
    }

    #[test]
    fn days_ago_range() {
        assert!(check_days_ago(0).is_ok());
        assert!(check_days_ago(3).is_ok());
        assert!(check_days_ago(4).is_err());
        assert!(check_days_ago(-1).is_err());
    }

    #[test]
    fn specified_past_is_kept() {
        let now = at(12, 0);
        let t = specified(&now, 0, 8, 20, 600).unwrap();
        assert_eq!(t.ms, at(8, 20).timestamp_millis());
        assert!(!t.clamped);
        let y = specified(&now, 1, 8, 20, 600).unwrap();
        assert_eq!(y.ms, at(8, 20).timestamp_millis() - 86_400_000);
    }

    #[test]
    fn specified_future_or_overlapping_is_clamped() {
        let now = at(12, 0);
        let now_ms = now.timestamp_millis();
        // 未来时刻
        let t = specified(&now, 0, 18, 0, 600).unwrap();
        assert!(t.clamped);
        assert!(ends_by_now(t, now_ms, 600));
        assert_eq!(t.ms, latest_start_ms(now_ms, 600));
        // 已过开始时刻但结束会越过当前
        let t = specified(&now, 0, 11, 55, 600).unwrap();
        assert!(t.clamped);
        assert!(ends_by_now(t, now_ms, 600));
        assert!(specified(&now, 4, 8, 0, 600).is_err());
    }

    #[test]
    fn ago_rules() {
        let now_ms = at(12, 0).timestamp_millis();
        let t = ago(now_ms, 60, 600).unwrap();
        assert_eq!(t.ms, now_ms - 3_600_000);
        assert!(!t.clamped);
        // 5 分钟前开跑、用时 10 分钟 → 必须提前
        let t = ago(now_ms, 5, 600).unwrap();
        assert!(t.clamped && ends_by_now(t, now_ms, 600));
        assert!(ago(now_ms, 0, 600).is_err());
        assert!(ago(now_ms, -5, 600).is_err());
        assert!(ago(now_ms, MAX_AGO_MIN + 1, 600).is_err());
    }

    #[test]
    fn random_default_never_ends_in_future() {
        let now_ms = at(12, 0).timestamp_millis();
        // 长跑（80 分钟）超过默认 30 分钟下限
        for r in [0.0, 0.01, 0.5, 0.999, 1.0, f64::NAN] {
            for dur in [0, 480, 4_800, 20_000] {
                let t = random_ago(now_ms, dur, r);
                assert!(ends_by_now(t, now_ms, dur), "r={r} dur={dur}");
                assert!(t.ms >= now_ms - DEFAULT_AGO_MAX * 60_000 - dur * 1000 - JITTER_MARGIN_MS);
            }
        }
        let t = random_ago(now_ms, 480, 0.0);
        assert_eq!(t.ms, now_ms - DEFAULT_AGO_MIN * 60_000);
        assert!(!t.clamped);
    }
}
