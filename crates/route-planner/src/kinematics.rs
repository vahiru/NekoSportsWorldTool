//! 运动学速度剖面：OU 配速 + 弯道预判刹车（有限减速度 lookahead）。
//!
//! 与逐点硬限速的区别：真人不会在入弯瞬间才减速，而是在进入弯道前一段距离
//! 就以有限制动减速度（约 1.5 m/s²）开始收速。这里对每个弧长 s 向后看
//! `lookahead_m` 范围内的曲率限速，并施加 `v_now² ≤ v_corner² + 2·b·d` 的
//! 可刹停约束，得到平滑且带预判的速度剖面。

use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::smooth::turn_speed_limit;
use crate::Route;

#[derive(Clone, Copy, Debug)]
pub struct KinParams {
    /// 最大向心加速度 m/s²（弯道限速上限）。
    pub a_c_max: f64,
    /// 最大制动减速度 m/s²（人体可接受的收速斜率）。
    pub brake_max: f64,
    /// 弯道预判前瞻距离（米）。
    pub lookahead_m: f64,
    /// 热身时长（秒），起点从 0.85× 匀速 ramp 到 1.0×。
    pub warmup_s: f64,
    /// OU 均值回归速率 θ（越小越持久）。
    pub theta: f64,
    /// OU 波动率 σ（m/s / √s）。
    pub sigma: f64,
}

impl Default for KinParams {
    fn default() -> Self {
        KinParams {
            a_c_max: 2.5,
            brake_max: 1.5,
            lookahead_m: 40.0,
            warmup_s: 20.0,
            theta: 0.12,
            sigma: 0.06,
        }
    }
}

/// 从弧长 s 出发，在 lookahead 范围内取最小的「可刹停速度」。
///
/// 对每个前方曲率限速 v_corner(q)（距离 d=q.s-s），当前速度最多允许
/// `sqrt(v_corner² + 2·b·d)`，取所有候选的最小值。
///
/// 路线是被重复跑的闭环：`s` 按圈长取模，前瞻窗口跨过闭环接缝时继续看下一圈
/// 开头的弯道。`route.points` 按 `s` 升序，起点用二分查找定位。
pub fn speed_limit_ahead(route: &Route, s: f64, p: &KinParams) -> f64 {
    let pts = &route.points;
    let len = route.length_m;
    let mut v_max = f64::INFINITY;
    if pts.is_empty() || !s.is_finite() {
        return v_max;
    }
    let looped = len.is_finite() && len > 0.0;
    let s0 = if looped { s.rem_euclid(len) } else { s };
    let mut offset = 0.0;
    let mut first = pts.partition_point(|q| q.s <= s0);
    loop {
        for pt in &pts[first..] {
            let d = pt.s + offset - s0;
            if d > p.lookahead_m {
                break;
            }
            if d <= 0.0 {
                continue;
            }
            let vc = turn_speed_limit(p.a_c_max, pt.curvature.abs());
            if vc.is_finite() {
                let ok = (vc * vc + 2.0 * p.brake_max * d).sqrt();
                if ok < v_max {
                    v_max = ok;
                }
            }
        }
        offset += len;
        if !looped || offset - s0 > p.lookahead_m {
            return v_max;
        }
        first = 0;
    }
}

/// 生成速度序列：OU 均值回归 + 热身 ramp + 弯道预判限速 + 精确命中目标距离。
///
/// 最终逐点速度被约束在 `[floor, min(ceil, 弯道限速)]` 内，并等比分摊使总距离
/// 精确等于 `target_dist`（越界点钳在 cap，剩余差量由未饱和点承担）。
#[allow(clippy::too_many_arguments)]
pub fn pace_profile(
    route: &Route,
    v0: f64,
    dts: &[f64],
    target_dist: f64,
    seed: u64,
    p: &KinParams,
    floor: f64,
    ceil: f64,
) -> Vec<f64> {
    use rand_distr::{Distribution, Normal};

    let n = dts.len();
    let mut rng = StdRng::seed_from_u64(seed);
    let normal = Normal::<f64>::new(0.0, 1.0).unwrap();

    // ① OU 均值回归 + 热身 ramp
    let mut w = Vec::with_capacity(n);
    let mut v = v0;
    let mut t = 0.0;
    for &dt in dts {
        let ramp = if t < p.warmup_s {
            0.85 + 0.15 * (t / p.warmup_s.max(1.0))
        } else {
            1.0
        };
        let mu = v0 * ramp;
        v = v + p.theta * (mu - v) * dt + p.sigma * dt.sqrt() * normal.sample(&mut rng);
        v = v.clamp(floor, ceil);
        w.push(v);
        t += dt;
    }

    // ② 弧长近似（未限速前）
    let mut s_of = Vec::with_capacity(n);
    let mut s = 0.0;
    for i in 0..n {
        s_of.push(s);
        s += w[i] * dts[i];
    }

    // ③ 弯道预判限速 cap（钳到有效窗口内）
    let mut caps = Vec::with_capacity(n);
    for i in 0..n {
        let cap = speed_limit_ahead(route, s_of[i], p).clamp(floor, ceil);
        w[i] = w[i].min(cap);
        caps.push(cap);
    }

    // ④ 精确命中目标距离（尊重逐点 cap）
    fit_capped(&mut w, dts, target_dist, floor, &caps);
    w
}

/// 等比缩放逐点速度至目标总距：越界点钳在 `[floor, caps[i]]`，剩余差量由未饱和点
/// 分摊（迭代收敛）。任何一点的瞬时配速都不会越出有效窗口。
pub fn fit_capped(w: &mut [f64], dts: &[f64], target: f64, floor: f64, caps: &[f64]) {
    for _ in 0..64 {
        let cur: f64 = w.iter().zip(dts).map(|(x, dt)| x * dt).sum();
        if (cur - target).abs() <= 0.5 {
            break;
        }
        let k = target / cur;
        for i in 0..w.len() {
            w[i] = (w[i] * k).clamp(floor, caps[i]);
        }
    }
    for i in 0..w.len() {
        w[i] = w[i].clamp(floor, caps[i]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoutePoint;

    /// 100 m loop with one tight corner at s=50.
    fn loop_with_corner() -> Route {
        let points = (0..=100)
            .map(|i| RoutePoint {
                lon: 0.0,
                lat: 0.0,
                s: i as f64,
                curvature: if i == 50 { 0.5 } else { 0.0 },
            })
            .collect();
        Route {
            points,
            length_m: 100.0,
        }
    }

    fn limit(curvature: f64, d: f64, p: &KinParams) -> f64 {
        (turn_speed_limit(p.a_c_max, curvature).powi(2) + 2.0 * p.brake_max * d).sqrt()
    }

    #[test]
    fn corner_limits_repeat_on_every_lap_and_across_the_seam() {
        let route = loop_with_corner();
        let p = KinParams::default();
        let lap1 = speed_limit_ahead(&route, 45.0, &p);
        assert!((lap1 - limit(0.5, 5.0, &p)).abs() < 1e-9);
        for lap in 1..5 {
            let s = 45.0 + lap as f64 * 100.0;
            assert!(
                (speed_limit_ahead(&route, s, &p) - lap1).abs() < 1e-9,
                "lap {lap}"
            );
        }
        assert!(speed_limit_ahead(&route, 160.0, &p).is_infinite());
        // The corner 10 m into the next lap is visible from 5 m before the seam.
        let mut seam = loop_with_corner();
        for q in &mut seam.points {
            q.curvature = if q.s == 10.0 { 0.5 } else { 0.0 };
        }
        let v = speed_limit_ahead(&seam, 195.0, &p);
        assert!((v - limit(0.5, 15.0, &p)).abs() < 1e-9, "{v}");
        // A loop shorter than the look-ahead sees its corner on several future laps.
        let short = Route {
            points: [0.0, 5.0, 10.0]
                .iter()
                .map(|&s| RoutePoint {
                    lon: 0.0,
                    lat: 0.0,
                    s,
                    curvature: if s == 5.0 { 0.5 } else { 0.0 },
                })
                .collect(),
            length_m: 10.0,
        };
        let v = speed_limit_ahead(&short, 18.0, &p);
        assert!((v - limit(0.5, 7.0, &p)).abs() < 1e-9, "{v}");
    }

    #[test]
    fn pace_profile_brakes_for_the_corner_on_every_lap() {
        let route = loop_with_corner();
        let p = KinParams::default();
        let dts = vec![0.25; 500];
        let w = pace_profile(&route, 4.0, &dts, 500.0, 3, &p, 1.0, 6.0);
        // Group braking samples (well below the ~4 m/s cruise) into separate passes.
        let mut passes = 0;
        let mut last = None;
        for (i, v) in w.iter().enumerate() {
            if *v < 3.0 {
                if last.is_none_or(|j| i - j > 40) {
                    passes += 1;
                }
                last = Some(i);
            }
        }
        assert!(passes >= 4, "corner braking only on {passes} laps");
    }
}
