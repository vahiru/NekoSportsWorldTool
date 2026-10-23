//! 两种生成器（经典环 / 真实道路）共用的采样步骤。
//!
//! 每个函数内部的随机数调用顺序与原内联实现逐一对应：调用方必须在原位置调用，
//! 才能保持相同种子的输出不变。
#![allow(non_snake_case)]

use super::geom::{fmt_gain_time, round_to, Rng};
use super::model::{GenPoint, Track};

/// 异常点（-1）位置漂移上限（米）。
pub const MAX_EXCEPTION_DRIFT_M: f64 = 6.0;
/// 异常点 GPS 瞬时速度上限（km/h）。
pub const MAX_EXCEPTION_GPS_SPEED_KMH: f64 = 6.0;

/// 时间网格：主 5s（80%），其余 1-8s；返回 (采样时刻, 各点时长)。`dur_f` 必须为正。
pub fn time_grid(rng: &mut Rng, dur_f: f64) -> (Vec<f64>, Vec<f64>) {
    let mut times = Vec::new();
    let mut t = 0.0;
    while t < dur_f {
        times.push(t);
        t += if rng.random() < 0.80 {
            5.0
        } else {
            rng.choice(&[1.0, 2.0, 3.0, 4.0, 6.0, 7.0, 8.0])
        };
    }
    let n = times.len();
    let mut dts: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    dts.push(1.0f64.max(dur_f - times[n - 1]));
    (times, dts)
}

/// 点位类型 (type, locType)：正常 3/0、异常 -1、偶发 1/2，漂移后偶发 7/8，禁止连续异常。
pub fn assign_kinds(rng: &mut Rng, n: usize) -> Vec<(i64, i64)> {
    let tl_w = [
        ((-1i64, 4i64), 54u32),
        ((-1, 1), 27),
        ((-1, 12), 13),
        ((-1, 5), 3),
        ((-1, 6), 2),
    ];
    let mut kinds: Vec<(i64, i64)> = Vec::with_capacity(n);
    for _ in 0..n {
        let u = rng.random();
        if u < 0.39 {
            kinds.push((3, 1));
        } else if u < 0.93 {
            kinds.push((0, 1));
        } else if u < 0.96 {
            kinds.push(rng.weighted(&tl_w));
        } else {
            kinds.push((rng.choice(&[1, 1, 1, 1, 2, 2]), 1));
        }
    }
    for i in 1..kinds.len() {
        let prev_drift = (-1 == kinds[i - 1].0) || kinds[i - 1].0 == 5 || kinds[i - 1].0 == 6;
        if kinds[i].0 != -1 && prev_drift && rng.random() < 0.08 {
            kinds[i] = (if rng.random() < 0.75 { 7 } else { 8 }, 1);
        }
    }
    for i in 1..n {
        if kinds[i].0 == -1 && kinds[i - 1].0 == -1 {
            kinds[i] = (rng.choice(&[3, 0]), 1);
        }
    }
    kinds
}

/// 距离只由正常点（索引 ≥2）承担，按速度曲线比例分摊并精确归一到 `dist`。
pub fn distribute(kinds: &[(i64, i64)], seg_dist: &[f64], dist: f64) -> Vec<f64> {
    let n = kinds.len();
    let normal_idx: Vec<usize> = (0..n).filter(|&i| kinds[i].0 != -1 && i > 1).collect();
    let share: f64 = normal_idx.iter().map(|&i| seg_dist[i]).sum();
    let share = if share == 0.0 { 1.0 } else { share };
    let mut disp_of = vec![0.0f64; n];
    for &i in &normal_idx {
        disp_of[i] = seg_dist[i] * (dist / share);
    }
    disp_of
}

/// 海拔随机游走：均值回归到 82m，总波动幅度与时长无关。
pub struct Altitude {
    alt: f64,
    sigma: f64,
}

impl Altitude {
    pub fn new(rng: &mut Rng, dur_f: f64) -> Self {
        let alt = 82.0 + rng.uniform(-1.0, 1.0);
        let n_est = 1.max((dur_f / 5.0) as i64);
        let sigma = rng.uniform(3.8, 6.2) / (0.40 * n_est as f64);
        Self { alt, sigma }
    }
    pub fn step(&mut self, rng: &mut Rng) -> f64 {
        self.alt += 0.04 * (82.0 - self.alt) + rng.gauss(0.0, self.sigma);
        self.alt
    }
}

/// 正常点的定位精度与状态。
pub fn normal_fix(rng: &mut Rng, typ: i64) -> (f64, i64) {
    let rad = round_to(
        if typ == 3 {
            rng.uniform(1.4, 5.1)
        } else {
            rng.uniform(1.4, 2.4)
        },
        2,
    );
    let state = if typ == 0 {
        rng.weighted(&[(1, 145), (2, 45), (3, 164)])
    } else {
        rng.weighted(&[(1, 145), (2, 256), (3, 151)])
    };
    (rad, state)
}

/// 异常点的局部漂移：(位移米, 方向)；20% 为零位移（精确复用上一点坐标）。
pub fn exception_drift(rng: &mut Rng) -> (f64, Option<f64>) {
    let d = if rng.random() >= 0.20 {
        rng.uniform(0.0, MAX_EXCEPTION_DRIFT_M)
    } else {
        0.0
    };
    let ang = (d > 0.0).then(|| rng.uniform(0.0, std::f64::consts::TAU));
    (d, ang)
}

/// 异常点的定位精度与状态（须在 [`exception_drift`] 之后调用）。
pub fn exception_fix(rng: &mut Rng) -> (f64, i64) {
    let rad = round_to(rng.uniform(1.6, 10.0), 2);
    let state = rng.weighted(&[(1, 102), (2, 124), (3, 136)]);
    (rad, state)
}

/// 单个采样点的确定量（随机量在 [`emit_point`] 内按固定顺序抽取）。
pub struct Sample {
    pub index: usize,
    pub kind: (i64, i64),
    /// 已按调用方规则取整的 BD 坐标。
    pub g_lat: f64,
    pub g_lng: f64,
    /// 无噪声的前进方位角（度）。
    pub heading_deg: f64,
    pub d_step: f64,
    pub dt: f64,
    pub t_acc: f64,
    pub dist_acc: f64,
    pub steps_acc: f64,
    pub alt: f64,
    pub radius: f64,
    pub state: i64,
}

/// 组装协议点：方位噪声 → 速度 → bdS/bdG/count（随机顺序固定）。
pub fn emit_point(rng: &mut Rng, s: &Sample, start_ms: i64) -> GenPoint {
    let (typ, lt) = s.kind;
    let brg = (s.heading_deg + rng.gauss(0.0, 35.0)).rem_euclid(360.0);
    // 异常点（-1）：avgSpeed 为累计均值（真人与此一致，不为 0）；
    // GPS 瞬时速度限制在低速范围，避免额外制造尖峰
    let (avg_sp, gps_speed) = if typ == -1 {
        let avg = round_to(s.dist_acc / s.t_acc.max(1.0), 4);
        let gps = rng.uniform(0.5, MAX_EXCEPTION_GPS_SPEED_KMH);
        (avg, round_to(gps, 4))
    } else {
        let avg = round_to(s.d_step / s.dt, 4);
        let kmh = avg * 3.6;
        let sigma = (kmh * 0.08).max(0.05);
        let gps = if rng.random() < 0.20 {
            0.0
        } else {
            round_to((kmh + rng.gauss(0.0, sigma)).max(0.0), 4)
        };
        (avg, gps)
    };
    let gain_ms = start_ms + (s.t_acc * 1000.0) as i64;
    GenPoint {
        id: s.index as i64 + 1,
        flag: start_ms,
        lat: -1.0,
        lng: -1.0,
        gLat: s.g_lat,
        gLng: s.g_lng,
        speed: round_to(gps_speed, 4),
        avgSpeed: avg_sp,
        radius: s.radius,
        accuracy: s.radius,
        ptype: typ,
        locType: lt,
        hasAltitude: true,
        totalTime: round_to(s.t_acc, 0) as i64,
        totalDis: round_to(s.dist_acc, 4),
        validDis: round_to(s.dist_acc, 4),
        validTime: round_to(s.t_acc, 0) as i64,
        steps: s.steps_acc as i64,
        stepDistance: 0.0,
        gainTime: fmt_gain_time(gain_ms),
        gainTimeMs: gain_ms,
        queueNum: 0,
        coorType: "gcj02".into(),
        bdA: round_to(s.alt, 2),
        bdD: round_to(brg, 2),
        bdS: round_to((avg_sp * rng.uniform(0.6, 0.95)).max(0.0), 3),
        bdG: rng.choice(&[1, 1, 1, -1]),
        count: rng.randint(20, 88),
        dtr: 0.0,
        state: s.state,
        locationId: String::new(),
    }
}

/// 最终点集 → 轨迹（总量由终点累计值派生，见 `stats::rebuild`）。
pub fn assemble(
    locs: Vec<GenPoint>,
    t_acc: f64,
    dist: f64,
    steps_acc: f64,
    start_ms: i64,
) -> Track {
    let total_dis = round_to(dist, 3);
    let mut track = Track {
        simulation: None,
        totalTime: round_to(t_acc, 0) as i64,
        totalDistance: total_dis,
        validDistance: total_dis,
        validTime: round_to(t_acc, 0) as i64,
        startTime: start_ms,
        startLatitude: locs[0].gLat,
        startLongitude: locs[0].gLng,
        totalSteps: steps_acc as i64,
        locations: locs,
        speedPerTenSec: Vec::new(),
        stepsPerTenSec: Vec::new(),
        segments: Vec::new(),
    };
    super::stats::rebuild(&mut track);
    track
}
