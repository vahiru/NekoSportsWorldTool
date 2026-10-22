//! 自然轨迹生成器。
//!
//! 画像驱动：打卡点拟合闭合环（Catmull-Rom，每段 18 采样）→ 弧长表；
//! 采样间隔主 5s（80%）；速度曲线 = ramp × 疲劳 × 三正弦 × 余弦凹陷 × 微噪；
//! 正常点按速度曲线分配位移并归一到精确总距离；异常点（-1）零位移/跳变；
//! 哨兵点（首 type∈{0,7}、索引1 type=5、末 type=6）；结尾断崖；点位吸附。

use super::geom::{dist2_m, make_point_ring, ring_point_at, round_to, LocalFrame, Rng};
use super::model::{GenPoint, Track};
use super::postfix::apply_post_fixes;
use super::synth::{self, Altitude, Sample};

/// 有效配速窗口（m/s），保留原有生成器边界。
pub const SPEED_FLOOR: f64 = 1.90;
pub const SPEED_CEIL: f64 = 6.30;

/// 生成参数校验：距离/时长为正、点位非空且坐标有限。
pub(super) fn validate_inputs(dist: f64, dur: i64, points: &[(f64, f64)]) -> Result<(), String> {
    if dur <= 0 || !dist.is_finite() || dist <= 0.0 {
        return Err("距离和时长必须为正数".into());
    }
    if points.is_empty() {
        return Err("没有路线点位，无法生成轨迹".into());
    }
    if points.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return Err("点位坐标无效，无法生成轨迹".into());
    }
    Ok(())
}

/// 轨迹生成主入口（参数无效时返回空轨迹，`validate_consistency` 会拒绝它）。
///
/// 需要错误原因时用 [`try_build`]。
pub fn build(
    dist: f64,
    dur: i64,
    seed: u64,
    center: (f64, f64),
    start_ms: i64,
    points_bd: &[(f64, f64)],
) -> Track {
    try_build(dist, dur, seed, center, start_ms, points_bd).unwrap_or_else(|_| Track {
        totalTime: 0,
        totalDistance: 0.0,
        validDistance: 0.0,
        validTime: 0,
        startTime: start_ms,
        startLatitude: 0.0,
        startLongitude: 0.0,
        locations: Vec::new(),
        totalSteps: 0,
        speedPerTenSec: Vec::new(),
        stepsPerTenSec: Vec::new(),
        segments: Vec::new(),
    })
}

/// 轨迹生成主入口。points_bd 为 BD 系打卡点。
pub fn try_build(
    dist: f64,
    dur: i64,
    seed: u64,
    _center: (f64, f64),
    start_ms: i64,
    points_bd: &[(f64, f64)],
) -> Result<Track, String> {
    validate_inputs(dist, dur, points_bd)?;
    let mut rng = Rng::new(seed);
    let dur_f = dur as f64;
    let (dense, arcs, pc) = make_point_ring(points_bd);
    let frame = LocalFrame::new(pc.0, pc.1);
    let direction: f64 = rng.choice(&[1.0, -1.0]);
    let s0 = rng.uniform(0.0, *arcs.last().unwrap_or(&400.0));
    let phase_v = rng.uniform(0.0, std::f64::consts::TAU);
    let phase_l = rng.uniform(0.0, std::f64::consts::TAU);

    let (times, dts) = synth::time_grid(&mut rng, dur_f);
    let n = times.len();

    let n_dips = rng.choice(&[1, 1, 2]);
    let mut dips = Vec::new();
    for _ in 0..n_dips {
        dips.push((
            rng.uniform(0.15, 0.75) * dur_f,
            rng.uniform(25.0, 55.0),
            rng.uniform(0.08, 0.16),
        ));
    }
    let dip_factor = |t: f64| -> f64 {
        let mut f = 1.0;
        for &(c, hw, d) in &dips {
            if (t - c).abs() < hw {
                f *= 1.0 - d * 0.5 * (1.0 + std::f64::consts::PI * (t - c) / hw).cos();
            }
        }
        f
    };
    let base = dist / dur_f;
    let mut w = Vec::with_capacity(n);
    for &tt in &times {
        let ramp = if tt < 8.0 {
            (0.85 + 0.15 * (tt / 1.0f64.max(8.0f64.min(dur_f / 20.0)))).min(1.0)
        } else {
            1.0
        } * if tt > dur_f - 8.0 {
            1.0 + 0.03 * (tt - (dur_f - 8.0)) / 8.0
        } else {
            1.0
        };
        let km_done = (tt / dur_f) * dist / 1000.0;
        let fatigue = if km_done <= 0.5 {
            1.03
        } else {
            (1.03 - 0.06 * (km_done - 0.5)).max(0.80)
        };
        let wave = (1.0
            + 0.010 * (std::f64::consts::TAU * tt / 115.0 + phase_v).sin()
            + 0.035 * (std::f64::consts::TAU * tt / 47.0 + phase_v * 2.3).sin()
            + 0.015 * (std::f64::consts::TAU * tt / 19.0 + phase_v * 3.7).sin())
            * dip_factor(tt);
        let noise = 1.0 + rng.gauss(0.0, 0.008);
        w.push(base * ramp * fatigue * wave * noise);
    }
    // 逐点速度全部约束在有效配速窗口内，并精确命中目标距离
    route_planner::kinematics::fit_capped(&mut w, &dts, dist, SPEED_FLOOR, &vec![SPEED_CEIL; n]);
    let seg_dist: Vec<f64> = (0..n).map(|i| w[i] * dts[i]).collect();

    let kinds = synth::assign_kinds(&mut rng, n);
    let disp_of = synth::distribute(&kinds, &seg_dist, dist);

    let mut locs: Vec<GenPoint> = Vec::with_capacity(n);
    let mut s = s0;
    let mut t_acc = 0.0f64;
    let mut dist_acc = 0.0f64;
    let mut steps_acc = 0.0f64;
    let mut jx = 0.0f64;
    let mut jy = 0.0f64;
    let mut altitude = Altitude::new(&mut rng, dur_f);
    let pos = |ss: f64| ring_point_at(&dense, &arcs, ss);
    for i in 0..n {
        let dt = dts[i];
        let typ = kinds[i].0;
        t_acc += dt;
        let mut d_step = 0.0f64;
        if typ != -1 {
            d_step = disp_of[i];
            s += direction * d_step;
        }
        let (x, y) = pos(s);
        let (px, py, (rad, state)) = if typ != -1 {
            jx = 0.72 * jx + rng.gauss(0.0, 0.75);
            jy = 0.72 * jy + rng.gauss(0.0, 0.75);
            (x + jx, y + jy, synth::normal_fix(&mut rng, typ))
        } else {
            let (drift, ang) = synth::exception_drift(&mut rng);
            d_step = drift;
            let (px, py) = match (ang, locs.last()) {
                (Some(ang), _) => (x + drift * ang.sin(), y + drift * ang.cos()),
                // 零位移：精确复制上一点坐标（BD→平面）
                (None, Some(last)) => frame.to_xy(last.gLat, last.gLng),
                (None, None) => (x + jx, y + jy),
            };
            (px, py, synth::exception_fix(&mut rng))
        };
        if typ != -1 {
            dist_acc += d_step; // 异常点漂移不计入累计距离
        }
        let (lat, lng) = frame.to_bd(px, py);
        let alt = altitude.step(&mut rng);
        let v_now = dist / dur_f;
        let stride_target = 0.62
            + 0.17 * v_now
            + 0.03 * (std::f64::consts::TAU * t_acc / 200.0 + phase_l).sin()
            + rng.gauss(0.0, 0.008);
        let v_cad = v_now * (1.0 + 0.03 * (std::f64::consts::TAU * t_acc / 70.0 + phase_v).sin());
        let cad = (v_cad / stride_target * 60.0).clamp(100.0, 200.0);
        steps_acc += cad / 60.0 * dt;
        let nxt = pos(s + direction * 2.0);
        let sample = Sample {
            index: i,
            kind: kinds[i],
            g_lat: round_to(lat, 7),
            g_lng: round_to(lng, 7),
            heading_deg: (nxt.0 - x).atan2(nxt.1 - y).to_degrees(),
            d_step,
            dt,
            t_acc,
            dist_acc,
            steps_acc,
            alt,
            radius: rad,
            state,
        };
        locs.push(synth::emit_point(&mut rng, &sample, start_ms));
    }
    apply_post_fixes(&mut locs, &mut rng, start_ms);

    // 点位吸附：<40m 精确落位
    for pl in points_bd {
        let mut best_i = None;
        let mut best_d = 1e18f64;
        for (i, q) in locs.iter().enumerate().filter(|(_, q)| q.ptype != -1) {
            let dd = dist2_m((q.gLat, q.gLng), *pl);
            if dd < best_d {
                best_d = dd;
                best_i = Some(i);
            }
        }
        if let Some(i) = best_i {
            if best_d < 40.0 * 40.0 {
                locs[i].gLat = round_to(pl.0, 7);
                locs[i].gLng = round_to(pl.1, 7);
            }
        }
    }

    Ok(synth::assemble(locs, t_acc, dist, steps_acc, start_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_inputs_are_errors_not_panics() {
        let pts = [(39.9, 116.4), (39.901, 116.401)];
        for (dist, dur, points) in [
            (1000.0, 0, &pts[..]),
            (1000.0, -5, &pts[..]),
            (0.0, 400, &pts[..]),
            (f64::NAN, 400, &pts[..]),
            (1000.0, 400, &[][..]),
            (1000.0, 400, &[(f64::NAN, 116.4)][..]),
        ] {
            assert!(try_build(dist, dur, 1, (0.0, 0.0), 0, points).is_err());
            let empty = build(dist, dur, 1, (0.0, 0.0), 0, points);
            assert!(empty.locations.is_empty());
            assert!(empty.validate_consistency().is_err());
        }
        // Degenerate but positive inputs (a single sample, a single point) do not panic.
        assert!(try_build(5.0, 1, 1, (0.0, 0.0), 0, &pts).is_ok());
        assert!(try_build(500.0, 200, 1, (0.0, 0.0), 0, &pts[..1]).is_ok());
    }
}
