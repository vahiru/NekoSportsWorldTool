//! 跑步方案：运动量抽样（距离 / 配速 / 用时）与开始时刻规则，纯逻辑、不依赖 egui。
//!
//! 开始时间：日期（今天 / 1-3 天前）两种模式共用；随机模式在该日 7:00-20:00 内抽样，
//! 并保证「开始 + 用时 + 抖动余量」不越过当前时刻；指定模式按用户填写的时/分，
//! 整段跑步尚未结束时提前到最晚可行时刻。「换一版」只重掷运动量（随机模式同时换时刻）。
//!
//! 时间规则本身在 [`crate::start_time`]（CLI 共用）；这里只做方案状态的组合。
//! 所有函数显式接收 `now` 与随机源，调用方读一次真实时钟，测试用固定时刻。
// CLI 只用 `start_time`；方案状态目前只有 GUI 跑步页使用，但保持在两种特性组合下编译和测试。
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

use crate::start_time::{self, latest_start_ms};
use chrono::{DateTime, TimeZone};
use rand::Rng;

/// 开始时间模式：随机时刻。
pub const MODE_RANDOM: usize = 0;
/// 开始时间模式：指定时刻。
pub const MODE_SPECIFIED: usize = 1;

/// 跑步页上决定方案的输入：距离 / 配速范围与开始时间控件。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlanInputs {
    pub dist_min: f32,
    pub dist_max: f32,
    pub pace_min: f32,
    pub pace_max: f32,
    /// [`MODE_RANDOM`] 或 [`MODE_SPECIFIED`]
    pub start_mode: usize,
    pub days_ago: i64,
    pub hour: i64,
    pub minute: i64,
}

/// 一次提交的确定方案（进入页面/参数变更时抽样生成）。
#[derive(Debug, Clone)]
pub struct RunPlan {
    pub dist_min: f32,
    pub dist_max: f32,
    pub pace_min: f32,
    pub pace_max: f32,
    pub start_mode: usize,
    pub days_ago: i64,
    pub hour: i64,
    pub minute: i64,
    /// 公里
    pub dist: f64,
    /// 秒/km
    pub pace: f32,
    /// 秒
    pub dur: i64,
    pub start_ms: i64,
    /// 本次方案的随机种子（预览与提交共用；提交时仍须重新校验策略和点位）
    pub seed: u64,
}

impl PlanInputs {
    /// 距离/配速是否与方案一致（不一致需重掷运动参数）。
    pub fn shape_matches(&self, p: &RunPlan) -> bool {
        p.dist_min == self.dist_min
            && p.dist_max == self.dist_max
            && p.pace_min == self.pace_min
            && p.pace_max == self.pace_max
    }

    /// 开始时间控件是否与方案一致。
    ///
    /// 随机模式的时刻是抽样结果（不是输入），只比对日期；指定模式的时/分是用户输入，需回比。
    pub fn time_matches(&self, p: &RunPlan) -> bool {
        if p.start_mode != self.start_mode || p.days_ago != self.days_ago {
            return false;
        }
        self.start_mode == MODE_RANDOM || (p.hour == self.hour && p.minute == self.minute)
    }

    /// 按当前参数抽样运动量（距离 / 配速 / 用时）。
    pub fn sample_shape<R: Rng + ?Sized>(&self, rng: &mut R) -> (f64, f32, i64) {
        let (lo, hi) = (
            self.dist_min.min(self.dist_max),
            self.dist_min.max(self.dist_max),
        );
        let (plo, phi) = (
            self.pace_min.min(self.pace_max),
            self.pace_min.max(self.pace_max),
        );
        let pace = plo + (phi - plo) * rng.gen::<f32>();
        let dist = (lo + (hi - lo) * rng.gen::<f32>()) as f64;
        let dur = (dist * pace as f64).round() as i64;
        (dist, pace, dur)
    }

    /// 时长：已抽样的方案优先，否则按当前距离/配速区间上界估一个。
    pub fn dur_hint(&self, plan: Option<&RunPlan>) -> i64 {
        match plan {
            Some(p) => p.dur,
            None => {
                let hi = self.dist_min.max(self.dist_max) as f64;
                let phi = self.pace_min.max(self.pace_max) as f64;
                (hi * phi).round() as i64
            }
        }
    }

    /// 按当前模式与日期取一个开始时刻（随机模式抽样，指定模式采纳输入框），
    /// 两种模式都保证「开始 + `dur_s` + 抖动余量」不越过 `now`。
    pub fn pick_start<Tz: TimeZone, R: Rng + ?Sized>(
        &self,
        now: &DateTime<Tz>,
        dur_s: i64,
        rng: &mut R,
    ) -> i64 {
        if self.start_mode == MODE_RANDOM {
            start_time::random_in_window(now, self.days_ago, dur_s, rng.gen::<f64>())
        } else {
            start_time::specified_clamped(now, self.days_ago, self.hour, self.minute, dur_s)
        }
    }

    /// 把当前参数与已算好的量写成方案。
    fn write(&self, dist: f64, pace: f32, dur: i64, start_ms: i64, seed: u64) -> RunPlan {
        RunPlan {
            dist_min: self.dist_min,
            dist_max: self.dist_max,
            pace_min: self.pace_min,
            pace_max: self.pace_max,
            start_mode: self.start_mode,
            days_ago: self.days_ago,
            hour: self.hour,
            minute: self.minute,
            dist,
            pace,
            dur,
            start_ms,
            seed,
        }
    }

    /// 「换一版」：重掷运动量；随机模式下同时换一个开始时刻。
    ///
    /// 指定模式的时刻由用户填写，不碰它（整段跑步尚未结束时仍按用时提前）。
    pub fn regen<Tz: TimeZone, R: Rng + ?Sized>(&self, now: &DateTime<Tz>, rng: &mut R) -> RunPlan {
        let (dist, pace, dur) = self.sample_shape(rng);
        let start_ms = self.pick_start(now, dur, rng);
        self.write(dist, pace, dur, start_ms, rng.gen::<u64>())
    }

    /// 确保方案与当前输入一致（参数变更补齐 / 提交前兜底）。返回是否写入了新方案
    /// （运动量重抽，预览需要重算）；只改开始时刻时返回 false。
    ///
    /// 两个一致性判定都在改写方案「之前」求值：否则重抽运动量会把当前参数写进方案，
    /// 同帧内「距离 + 日期」一起改时日期变化将被漏掉。
    pub fn ensure<Tz: TimeZone, R: Rng + ?Sized>(
        &self,
        plan: &mut Option<RunPlan>,
        now: &DateTime<Tz>,
        rng: &mut R,
    ) -> bool {
        let shape_ok = plan.as_ref().map(|p| self.shape_matches(p)) == Some(true);
        let time_ok = plan.as_ref().map(|p| self.time_matches(p)) == Some(true);
        let mut wrote = false;
        if !shape_ok {
            // 运动参数变更：重抽距离/配速/用时，保留已选开始时刻
            let (dist, pace, dur) = self.sample_shape(rng);
            let start_ms = match (plan.as_ref(), self.start_mode) {
                // 随机模式沿用已抽样的时刻（仅按新用时收紧上限），避免拖动距离时时刻乱跳
                (Some(p), MODE_RANDOM) => {
                    p.start_ms.min(latest_start_ms(now.timestamp_millis(), dur))
                }
                _ => self.pick_start(now, dur, rng),
            };
            *plan = Some(self.write(dist, pace, dur, start_ms, rng.gen::<u64>()));
            wrote = true;
        }
        if !time_ok {
            // 日期 / 模式 / 时刻变更：只重算开始时刻，保留已抽样的运动量
            if let Some(p) = plan.as_mut() {
                p.start_ms = self.pick_start(now, p.dur, rng);
                p.start_mode = self.start_mode;
                p.days_ago = self.days_ago;
                p.hour = self.hour;
                p.minute = self.minute;
            }
        }
        wrote
    }

    /// 今天 + 指定时刻且整段跑步（含余量）尚未结束时，返回结束时刻（毫秒）。
    pub fn pending_specified_today<Tz: TimeZone>(
        &self,
        plan: Option<&RunPlan>,
        now: &DateTime<Tz>,
    ) -> Option<i64> {
        if self.start_mode != MODE_SPECIFIED || self.days_ago != 0 {
            return None;
        }
        start_time::specified_today_pending(now, self.hour, self.minute, self.dur_hint(plan))
    }

    /// 跑步页时间相关文案的下一次变化时刻：指定时刻的跑步结束、跨日（日期标签/随机窗口提示）。
    pub fn next_time_transition<Tz: TimeZone>(
        &self,
        plan: Option<&RunPlan>,
        now: &DateTime<Tz>,
    ) -> Option<i64> {
        match (
            self.pending_specified_today(plan, now),
            start_time::next_midnight(now),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 随机模式的抽样结果是否落在所选日期的 7:00-20:00 窗口内（无方案视为是）。
    pub fn random_window_ok<Tz: TimeZone>(
        &self,
        plan: Option<&RunPlan>,
        now: &DateTime<Tz>,
    ) -> bool {
        plan.is_none_or(|p| start_time::in_random_window(now, self.days_ago, p.start_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start_time::{JITTER_MARGIN_MS, RAND_HOUR_HI, RAND_HOUR_LO};
    use chrono::{Duration, FixedOffset, Timelike};
    use rand::{rngs::StdRng, SeedableRng};

    /// 固定 UTC+8（无夏令时）的 2026-10-22 h:m:s。
    fn at(h: u32, m: u32, s: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 22, h, m, s)
            .unwrap()
    }

    fn rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    /// 一份参数合理的输入（配速 6:00/km，距离 2 km 附近）。
    fn inputs(start_mode: usize, days_ago: i64) -> PlanInputs {
        PlanInputs {
            dist_min: 2.0,
            dist_max: 2.2,
            pace_min: 350.0,
            pace_max: 370.0,
            start_mode,
            days_ago,
            hour: 12,
            minute: 0,
        }
    }

    fn regen(i: &PlanInputs, now: &DateTime<FixedOffset>, r: &mut StdRng) -> RunPlan {
        i.regen(now, r)
    }

    fn local(now: &DateTime<FixedOffset>, ms: i64) -> DateTime<FixedOffset> {
        now.timezone().timestamp_millis_opt(ms).single().unwrap()
    }

    fn ms(now: &DateTime<FixedOffset>, days_ago: i64, h: u32, m: u32) -> i64 {
        (now.date_naive() - Duration::days(days_ago))
            .and_hms_opt(h, m, 0)
            .and_then(|t| now.timezone().from_local_datetime(&t).single())
            .unwrap()
            .timestamp_millis()
    }

    /// 整段跑步（含 flow.rs 的 0-4s 抖动余量）不越过当前时刻。
    fn assert_ends_by_now(p: &RunPlan, now: &DateTime<FixedOffset>) {
        assert!(
            p.start_ms + p.dur * 1000 + JITTER_MARGIN_MS <= now.timestamp_millis(),
            "开始 {} + 用时 {}s 越过当前时刻",
            p.start_ms,
            p.dur
        );
    }

    /// 指定模式下「换一版」只换运动量，用户填的时刻必须原样保留。
    #[test]
    fn shuffle_keeps_user_time_in_specified_mode() {
        let now = at(12, 0, 0);
        let mut r = rng();
        let mut i = inputs(MODE_SPECIFIED, 1);
        i.hour = 9;
        i.minute = 15;
        let want = ms(&now, 1, 9, 15);
        let mut dists = std::collections::HashSet::new();
        for _ in 0..40 {
            let p = regen(&i, &now, &mut r);
            assert_eq!((p.hour, p.minute, p.days_ago), (9, 15, 1));
            assert_eq!(p.start_ms, want, "「换一版」改动了用户填的时刻");
            dists.insert(p.dist.to_bits());
        }
        assert!(dists.len() > 1, "「换一版」40 次仍未改变距离");
    }

    /// 随机模式：时刻落在该日 7:00-20:00 内，且「换一版」每次都换新的。
    #[test]
    fn random_mode_within_window_and_varies() {
        let now = at(12, 0, 0);
        let mut r = rng();
        let i = inputs(MODE_RANDOM, 1);
        let lo = ms(&now, 1, RAND_HOUR_LO, 0);
        let hi = ms(&now, 1, RAND_HOUR_HI, 0);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..40 {
            let p = regen(&i, &now, &mut r);
            assert!((lo..=hi).contains(&p.start_ms), "{}", p.start_ms);
            assert!(i.random_window_ok(Some(&p), &now));
            seen.insert(p.start_ms);
        }
        assert!(seen.len() > 1, "随机模式 40 次仍未改变时刻");
    }

    /// 任何模式、任何日期下整段跑步都不越过当前时刻（regen 与 ensure 两条路径）。
    #[test]
    fn never_schedules_future_run() {
        let mut r = rng();
        for now in [at(0, 1, 0), at(7, 30, 0), at(12, 0, 0), at(23, 59, 59)] {
            for mode in [MODE_RANDOM, MODE_SPECIFIED] {
                for days_ago in 0..=3 {
                    let mut i = inputs(mode, days_ago);
                    i.hour = 23;
                    i.minute = 59;
                    for _ in 0..10 {
                        assert_ends_by_now(&regen(&i, &now, &mut r), &now);
                    }
                    let mut plan = Some(regen(&i, &now, &mut r));
                    i.dist_max = 5.0;
                    i.pace_max = 500.0;
                    assert!(i.ensure(&mut plan, &now, &mut r));
                    assert_ends_by_now(plan.as_ref().unwrap(), &now);
                }
            }
        }
    }

    /// 今天 + 已过去且整段已结束的指定时刻：原样提交。
    #[test]
    fn specified_today_past_time_is_kept() {
        let now = at(12, 0, 0);
        let mut i = inputs(MODE_SPECIFIED, 0);
        i.hour = 0;
        i.minute = 30;
        let p = regen(&i, &now, &mut rng());
        assert_eq!(p.start_ms, ms(&now, 0, 0, 30));
        assert_eq!(i.pending_specified_today(Some(&p), &now), None);
    }

    /// 今天 23:59（未来）：钳制到 `now - 用时 - 余量`，界面据此提示。
    #[test]
    fn specified_today_2359_is_clamped_and_pending() {
        let now = at(12, 0, 0);
        let mut i = inputs(MODE_SPECIFIED, 0);
        i.hour = 23;
        i.minute = 59;
        let p = regen(&i, &now, &mut rng());
        let now_ms = now.timestamp_millis();
        assert_eq!(p.start_ms, now_ms - p.dur * 1000 - JITTER_MARGIN_MS);
        assert_eq!(
            i.pending_specified_today(Some(&p), &now),
            Some(ms(&now, 0, 23, 59) + p.dur * 1000 + JITTER_MARGIN_MS)
        );
        // 23:59:30 时刻已开始但整段跑步尚未结束，仍提示
        let late = at(23, 59, 30);
        assert!(i.pending_specified_today(Some(&p), &late).is_some());
    }

    /// 指定时刻已开始但整段跑步尚未结束：同样提前到最晚可行时刻。
    #[test]
    fn specified_overlapping_now_is_clamped() {
        let mut i = inputs(MODE_SPECIFIED, 0);
        i.hour = 11;
        i.minute = 55;
        let now = at(12, 0, 0);
        let p = regen(&i, &now, &mut rng());
        assert!(p.dur > 300);
        assert_eq!(
            p.start_ms,
            latest_start_ms(now.timestamp_millis(), p.dur),
            "未按 min(填入, 现在 - 用时 - 余量) 处理"
        );
        // 只有今天才会提示
        i.days_ago = 1;
        assert_eq!(i.pending_specified_today(Some(&p), &now), None);
    }

    /// 刚过零点：今天的 7:00-20:00 尚未到达，随机模式退化为最晚可行时刻并被识别出来。
    #[test]
    fn random_today_just_after_midnight_falls_back_to_latest() {
        let now = at(0, 5, 0);
        let i = inputs(MODE_RANDOM, 0);
        let p = regen(&i, &now, &mut rng());
        assert_eq!(p.start_ms, latest_start_ms(now.timestamp_millis(), p.dur));
        assert!(!i.random_window_ok(Some(&p), &now), "凌晨不应判为在窗口内");
        // 用时超过零点以来的时间：开跑落在昨天
        assert!(local(&now, p.start_ms).date_naive() < now.date_naive());
    }

    /// 用时长于「窗口开始以来的时间」：抽样区间收缩到开头，仍不越过当前时刻。
    #[test]
    fn random_today_duration_longer_than_window_so_far() {
        // 7:10 开窗才 10 分钟，跑约 12 分钟：上限早于 7:00
        let now = at(7, 10, 0);
        let i = inputs(MODE_RANDOM, 0);
        let mut r = rng();
        for _ in 0..20 {
            let p = regen(&i, &now, &mut r);
            assert_eq!(p.start_ms, latest_start_ms(now.timestamp_millis(), p.dur));
            assert_ends_by_now(&p, &now);
            assert!(!i.random_window_ok(Some(&p), &now));
        }
        // 窗口已过一部分且足够长：落在 7:00 与上限之间
        let now = at(9, 0, 0);
        let lo = ms(&now, 0, RAND_HOUR_LO, 0);
        for _ in 0..20 {
            let p = regen(&i, &now, &mut r);
            let latest = latest_start_ms(now.timestamp_millis(), p.dur);
            assert!((lo..=latest).contains(&p.start_ms));
            assert!(i.random_window_ok(Some(&p), &now));
        }
    }

    /// 日期下拉 0-3 天前对两种模式都生效；越界值钳制到 0-3。
    #[test]
    fn days_ago_bounds_apply_to_both_modes() {
        let now = at(12, 0, 0);
        let mut r = rng();
        for mode in [MODE_RANDOM, MODE_SPECIFIED] {
            for (days_ago, want) in [(0, 0), (1, 1), (2, 2), (3, 3), (4, 3), (-1, 0)] {
                let mut i = inputs(mode, days_ago);
                i.hour = 8;
                let p = regen(&i, &now, &mut r);
                let day = local(&now, p.start_ms).date_naive();
                assert_eq!(
                    day,
                    now.date_naive() - Duration::days(want),
                    "mode={mode} days_ago={days_ago}"
                );
            }
        }
    }

    /// 改距离/配速只重抽运动量，不打乱指定时刻；随机模式沿用已抽样时刻。
    #[test]
    fn shape_change_keeps_start_time() {
        let now = at(12, 0, 0);
        let mut r = rng();
        for mode in [MODE_RANDOM, MODE_SPECIFIED] {
            let mut i = inputs(mode, 1);
            i.hour = 9;
            i.minute = 15;
            let mut plan = Some(regen(&i, &now, &mut r));
            let before = plan.clone().unwrap();
            i.dist_min = 3.0;
            i.dist_max = 3.5;
            assert!(i.ensure(&mut plan, &now, &mut r), "运动量变化应写入新方案");
            let after = plan.unwrap();
            assert_ne!((after.dist, after.pace), (before.dist, before.pace));
            assert_eq!(
                after.start_ms, before.start_ms,
                "mode={mode} 改距离改动了时刻"
            );
        }
    }

    /// 改时/分、切模式、改日期：只重算时刻，不重抽运动量，也不算写入新方案。
    #[test]
    fn time_edits_resync_time_only() {
        let now = at(12, 0, 0);
        let mut r = rng();
        let mut i = inputs(MODE_SPECIFIED, 1);
        let mut plan = Some(regen(&i, &now, &mut r));
        let before = plan.clone().unwrap();

        i.hour = 8;
        i.minute = 45;
        assert!(!i.ensure(&mut plan, &now, &mut r));
        let p = plan.clone().unwrap();
        assert_eq!((p.hour, p.minute), (8, 45));
        assert_eq!(p.start_ms, ms(&now, 1, 8, 45));
        assert_eq!(
            (p.dist, p.pace, p.dur),
            (before.dist, before.pace, before.dur)
        );

        i.start_mode = MODE_RANDOM;
        assert!(!i.ensure(&mut plan, &now, &mut r));
        let p = plan.clone().unwrap();
        assert_eq!(p.start_mode, MODE_RANDOM);
        assert_eq!(
            (p.dist, p.pace, p.dur),
            (before.dist, before.pace, before.dur)
        );
        assert_ends_by_now(&p, &now);

        i.days_ago = 3;
        assert!(!i.ensure(&mut plan, &now, &mut r));
        let p = plan.unwrap();
        assert_eq!(p.days_ago, 3);
        assert_eq!(
            local(&now, p.start_ms).date_naive(),
            now.date_naive() - Duration::days(3)
        );
    }

    /// 同一帧内「距离 + 日期」或「距离 + 时刻」一起改：两处变化都不能漏。
    #[test]
    fn simultaneous_shape_and_time_change() {
        let now = at(12, 0, 0);
        let mut r = rng();
        let mut i = inputs(MODE_RANDOM, 0);
        let mut plan = Some(regen(&i, &now, &mut r));
        i.dist_min = 4.0;
        i.dist_max = 4.5;
        i.days_ago = 2;
        i.ensure(&mut plan, &now, &mut r);
        let p = plan.clone().unwrap();
        assert_eq!(p.days_ago, 2, "日期变化被漏掉");
        assert!(p.dist >= 4.0, "距离变化被漏掉");
        assert_eq!(
            local(&now, p.start_ms).date_naive(),
            now.date_naive() - Duration::days(2)
        );

        let mut i = inputs(MODE_SPECIFIED, 1);
        let mut plan = Some(regen(&i, &now, &mut r));
        i.dist_min = 4.0;
        i.dist_max = 4.5;
        i.hour = 8;
        i.minute = 20;
        i.ensure(&mut plan, &now, &mut r);
        let p = plan.unwrap();
        assert_eq!((p.hour, p.minute), (8, 20), "时刻变化被漏掉");
        assert!(p.dist >= 4.0, "距离变化被漏掉");
        assert_eq!(p.start_ms, ms(&now, 1, 8, 20));
    }

    /// 改了运动量后，指定时刻按「新」用时钳制（不沿用旧方案的用时）。
    #[test]
    fn shape_change_clamps_specified_with_new_duration() {
        let now = at(12, 0, 0);
        let mut r = rng();
        let mut i = inputs(MODE_SPECIFIED, 0);
        i.hour = 11;
        i.minute = 0;
        let mut plan = Some(regen(&i, &now, &mut r));
        i.dist_min = 9.0;
        i.dist_max = 9.5;
        i.ensure(&mut plan, &now, &mut r);
        assert_ends_by_now(plan.as_ref().unwrap(), &now);
    }

    /// 反复 ensure 是幂等的。
    #[test]
    fn ensure_is_idempotent() {
        let now = at(12, 0, 0);
        let mut r = rng();
        for mode in [MODE_RANDOM, MODE_SPECIFIED] {
            let i = inputs(mode, 1);
            let mut plan = Some(regen(&i, &now, &mut r));
            let first = plan.clone().unwrap();
            for _ in 0..5 {
                assert!(!i.ensure(&mut plan, &now, &mut r));
            }
            let again = plan.unwrap();
            assert_eq!(
                (
                    again.dist,
                    again.pace,
                    again.dur,
                    again.start_ms,
                    again.seed
                ),
                (
                    first.dist,
                    first.pace,
                    first.dur,
                    first.start_ms,
                    first.seed
                ),
                "mode={mode} ensure 不稳定"
            );
        }
    }

    /// 没有方案时 ensure 生成一份完整方案。
    #[test]
    fn ensure_creates_missing_plan() {
        let now = at(12, 0, 0);
        let i = inputs(MODE_SPECIFIED, 2);
        let mut plan = None;
        assert!(i.ensure(&mut plan, &now, &mut rng()));
        let p = plan.unwrap();
        assert!(i.shape_matches(&p) && i.time_matches(&p));
        assert_eq!(p.start_ms, ms(&now, 2, 12, 0));
    }

    /// 时长估计：无方案时按区间上界估，有方案时取方案用时。
    #[test]
    fn dur_hint_prefers_plan() {
        let i = inputs(MODE_RANDOM, 0);
        assert_eq!(i.dur_hint(None), (2.2f32 as f64 * 370.0).round() as i64);
        let p = regen(&i, &at(12, 0, 0), &mut rng());
        assert_eq!(i.dur_hint(Some(&p)), p.dur);
    }

    /// 下一次文案变化：指定时刻的跑步结束或零点，取较早者。
    #[test]
    fn next_time_transition_rules() {
        let mut i = inputs(MODE_SPECIFIED, 0);
        i.hour = 10;
        i.minute = 1;
        let p = regen(&i, &at(12, 0, 0), &mut rng());
        let start = ms(&at(0, 0, 0), 0, 10, 1);
        let ends = start + p.dur * 1000 + JITTER_MARGIN_MS;
        let now = local(&at(0, 0, 0), start + p.dur * 1000);
        assert_eq!(i.next_time_transition(Some(&p), &now), Some(ends));
        // 跑步已结束：只剩零点
        let midnight = at(0, 0, 0).timestamp_millis() + 86_400_000;
        let after = local(&now, ends);
        assert_eq!(i.next_time_transition(Some(&p), &after), Some(midnight));
        // 其他日期 / 随机模式：只看零点
        i.days_ago = 1;
        assert_eq!(i.next_time_transition(Some(&p), &now), Some(midnight));
        let r = inputs(MODE_RANDOM, 0);
        assert_eq!(
            r.next_time_transition(Some(&p), &at(23, 59, 59)),
            Some(midnight)
        );
        // 23:59 指定：结束在零点之后，零点先到
        i.days_ago = 0;
        i.hour = 23;
        i.minute = 59;
        assert_eq!(
            i.next_time_transition(Some(&p), &at(23, 0, 0)),
            Some(midnight)
        );
    }

    /// 随机窗口判定：无方案视为在窗口内；判定按页面所选日期。
    #[test]
    fn random_window_detection() {
        let now = at(12, 0, 0);
        let i = inputs(MODE_RANDOM, 0);
        assert!(i.random_window_ok(None, &now));
        let mut p = regen(&i, &now, &mut rng());
        p.start_ms = ms(&now, 0, 3, 0);
        assert!(!i.random_window_ok(Some(&p), &now), "03:00 不在 7:00-20:00");
        p.start_ms = ms(&now, 0, 20, 30);
        assert!(i.random_window_ok(Some(&p), &now), "20 点整点小时计入窗口");
        p.start_ms = ms(&now, 1, 9, 0);
        assert!(!i.random_window_ok(Some(&p), &now), "日期不符");
        assert_eq!(local(&now, p.start_ms).hour(), 9);
    }
}
