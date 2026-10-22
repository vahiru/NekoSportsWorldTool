//! 跑步开始时间计算（纯函数，GUI 与 CLI 共用）。
//!
//! 规则：开始 + 用时 + 抖动余量 ≤ 当前时刻（flow.rs 提交时会给开始时间再加 0-4s
//! 抖动），超出时把开始时刻提前到上限并标记 `clamped`；非法输入返回错误，
//! 不再静默回退到当前时刻。
//!
//! 所有函数都显式接收当前时刻（`now`），不读系统时钟：调用方（GUI / CLI）读一次真实时钟，
//! 测试用固定时刻与固定时区。GUI 的跑步方案见 `crate::plan`。

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Timelike};

/// flow.rs 提交时给开始时间加 0-4s 抖动，预留 5s 余量。
pub const JITTER_MARGIN_MS: i64 = 5_000;
/// 最多往前 3 天（与 GUI 日期下拉一致）。
pub const MAX_DAYS_AGO: i64 = 3;
/// `--ago` 上限（分钟）：与 3 天上限一致。
pub const MAX_AGO_MIN: i64 = (MAX_DAYS_AGO + 1) * 24 * 60;
/// 默认随机窗口：30-300 分钟前。
pub const DEFAULT_AGO_MIN: i64 = 30;
pub const DEFAULT_AGO_MAX: i64 = 300;
/// GUI 随机模式的时刻窗口（含端点）：所选日期的 7:00-20:00。
pub const RAND_HOUR_LO: u32 = 7;
pub const RAND_HOUR_HI: u32 = 20;

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
        StartTime {
            ms: latest,
            clamped: true,
        }
    } else {
        StartTime {
            ms: start_ms,
            clamped: false,
        }
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
    let r = if r.is_finite() {
        r.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let span_ms = ((DEFAULT_AGO_MAX - DEFAULT_AGO_MIN) * 60_000) as f64;
    let start = now_ms - DEFAULT_AGO_MIN * 60_000 - (r * span_ms) as i64;
    clamp(start, now_ms, dur_s)
}

/// `days_ago` 天前（钳制到 0-3）的日历日期，按 `now` 所在时区计算。
pub fn day_of<Tz: TimeZone>(now: &DateTime<Tz>, days_ago: i64) -> NaiveDate {
    now.date_naive() - Duration::days(days_ago.clamp(0, MAX_DAYS_AGO))
}

/// `date` 当天 `hour:minute` 在 `now` 所在时区的时间戳；不存在或有歧义（夏令时）返回 None。
fn at_on<Tz: TimeZone>(now: &DateTime<Tz>, date: NaiveDate, hour: u32, minute: u32) -> Option<i64> {
    let naive = date.and_hms_opt(hour, minute, 0)?;
    now.timezone()
        .from_local_datetime(&naive)
        .single()
        .map(|t| t.timestamp_millis())
}

/// GUI 指定模式：输入先钳制到控件范围（天 0-3、时 0-23、分 0-59），再按 [`specified`] 计算。
///
/// 本地时区不存在的时刻（夏令时跳变）退化为开跑上限 [`latest_start_ms`]。
pub fn specified_clamped<Tz: TimeZone>(
    now: &DateTime<Tz>,
    days_ago: i64,
    hour: i64,
    minute: i64,
    dur_s: i64,
) -> i64 {
    specified(
        now,
        days_ago.clamp(0, MAX_DAYS_AGO),
        hour.clamp(0, 23) as u32,
        minute.clamp(0, 59) as u32,
        dur_s,
    )
    .map(|t| t.ms)
    .unwrap_or_else(|_| latest_start_ms(now.timestamp_millis(), dur_s))
}

/// GUI 随机模式：在 `days_ago` 天前的 7:00-20:00 内按 `r` ∈ [0,1) 取一个时刻，且不晚于
/// [`latest_start_ms`]。
///
/// 若该日窗口整体晚于上限（例如凌晨选「今天」），退化为上限，宁可贴近当前时间，
/// 也不产生「未来开跑」。
pub fn random_in_window<Tz: TimeZone>(
    now: &DateTime<Tz>,
    days_ago: i64,
    dur_s: i64,
    r: f64,
) -> i64 {
    let latest_ms = latest_start_ms(now.timestamp_millis(), dur_s);
    let day = day_of(now, days_ago);
    let (lo_ms, hi_ms) = match (
        at_on(now, day, RAND_HOUR_LO, 0),
        at_on(now, day, RAND_HOUR_HI, 0),
    ) {
        (Some(l), Some(h)) => (l, h),
        _ => return latest_ms,
    };
    let r = if r.is_finite() {
        r.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let hi_ms = hi_ms.min(latest_ms);
    let lo_ms = lo_ms.min(hi_ms);
    let span = (hi_ms - lo_ms).max(0) as f64;
    lo_ms + (r * span) as i64
}

/// `start_ms` 是否落在 `days_ago` 天前的 7:00-20:59（按小时判定，含 20 点整点小时）内。
///
/// 凌晨选「今天」时窗口尚未到达，[`random_in_window`] 会退化到贴近当前时刻，界面据此提示。
pub fn in_random_window<Tz: TimeZone>(now: &DateTime<Tz>, days_ago: i64, start_ms: i64) -> bool {
    let Some(t) = now.timezone().timestamp_millis_opt(start_ms).single() else {
        return true;
    };
    t.date_naive() == day_of(now, days_ago) && (RAND_HOUR_LO..=RAND_HOUR_HI).contains(&t.hour())
}

/// 今天的 `hour:minute` 开跑、整段跑步（含抖动余量）在 `now` 时尚未结束时，返回结束时刻（毫秒）。
pub fn specified_today_pending<Tz: TimeZone>(
    now: &DateTime<Tz>,
    hour: i64,
    minute: i64,
    dur_s: i64,
) -> Option<i64> {
    let at = at_on(
        now,
        now.date_naive(),
        hour.clamp(0, 23) as u32,
        minute.clamp(0, 59) as u32,
    )?;
    let ends = at + dur_s * 1000 + JITTER_MARGIN_MS;
    (ends > now.timestamp_millis()).then_some(ends)
}

/// `now` 之后的下一个本地零点（毫秒）。
pub fn next_midnight<Tz: TimeZone>(now: &DateTime<Tz>) -> Option<i64> {
    let t = now.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
    now.timezone()
        .from_local_datetime(&t)
        .earliest()
        .map(|t| t.timestamp_millis())
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
        for bad in [
            "24:00", "7:60", "7:xx", "ab", "", ":", "7:", "-1:00", "07:300", "7:5:0",
        ] {
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

    #[test]
    fn gui_specified_clamps_inputs_and_end() {
        let now = at(12, 0);
        let now_ms = now.timestamp_millis();
        // 越界输入按控件范围钳制：99 天 → 3 天，25:75 → 23:59（3 天前已过去，原样保留）
        assert_eq!(
            specified_clamped(&now, 99, 25, 75, 600),
            at(23, 59).timestamp_millis() - 3 * 86_400_000
        );
        assert_eq!(
            specified_clamped(&now, -1, 8, 0, 600),
            at(8, 0).timestamp_millis()
        );
        // 今天 23:59：钳制到 now - 用时 - 余量
        assert_eq!(
            specified_clamped(&now, 0, 23, 59, 600),
            latest_start_ms(now_ms, 600)
        );
    }

    #[test]
    fn random_window_bounds() {
        let now = at(12, 0);
        let day = |h: u32| at(h, 0).timestamp_millis() - 86_400_000;
        // 昨天：r=0 → 7:00，r=1 → 20:00，非法 r 退化为 0
        assert_eq!(random_in_window(&now, 1, 600, 0.0), day(RAND_HOUR_LO));
        assert_eq!(random_in_window(&now, 1, 600, 1.0), day(RAND_HOUR_HI));
        assert_eq!(random_in_window(&now, 1, 600, f64::NAN), day(RAND_HOUR_LO));
        assert!(in_random_window(&now, 1, day(RAND_HOUR_HI)));
        assert!(!in_random_window(&now, 0, day(RAND_HOUR_HI)), "日期不符");
        // 今天中午：上限收紧到 now - 用时 - 余量
        let latest = latest_start_ms(now.timestamp_millis(), 600);
        assert_eq!(random_in_window(&now, 0, 600, 1.0), latest);
        // 刚过零点：窗口未到，退化为上限，且不在窗口内
        let early = at(0, 1);
        let t = random_in_window(&early, 0, 600, 0.5);
        assert_eq!(t, latest_start_ms(early.timestamp_millis(), 600));
        assert!(!in_random_window(&early, 0, t));
    }

    #[test]
    fn dates_follow_the_given_offset_not_utc() {
        // UTC+8 的 01:00 在 UTC 仍是前一天；「今天」必须按本地日期算
        let now = at(1, 0);
        assert_eq!(day_of(&now, 0), now.date_naive());
        assert_eq!(day_of(&now, 5), now.date_naive() - Duration::days(3));
        assert_eq!(
            next_midnight(&now),
            Some(at(0, 0).timestamp_millis() + 86_400_000)
        );
        let west = FixedOffset::west_opt(5 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 21, 23, 30, 0)
            .unwrap();
        assert_eq!(day_of(&west, 0), west.date_naive());
        assert_eq!(
            next_midnight(&west),
            Some(west.timestamp_millis() + 30 * 60_000)
        );
    }

    #[test]
    fn specified_today_pending_edges() {
        // 23:59 开跑、用时 600 s：23:59:30 仍未结束；结束（含余量）那一刻起不再提示
        let ends = at(23, 59).timestamp_millis() + 600_000 + JITTER_MARGIN_MS;
        assert_eq!(specified_today_pending(&at(12, 0), 23, 59, 600), Some(ends));
        let late = at(23, 59) + Duration::seconds(30);
        assert_eq!(specified_today_pending(&late, 23, 59, 600), Some(ends));
        // 8:00 开跑早已结束
        assert_eq!(specified_today_pending(&at(12, 0), 8, 0, 600), None);
        // 恰好结束时刻：不再提示
        let done = at(11, 0) + Duration::milliseconds(600_000 + JITTER_MARGIN_MS);
        assert_eq!(specified_today_pending(&done, 11, 0, 600), None);
    }
}
