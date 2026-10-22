//! 轨迹几何与随机工具。
//!
//! round 封装、RNG、打卡点 Catmull-Rom 拟合环 + 弧长表 + 弧长插值、局部米制平面。
//! 经纬度距离与坐标基准换算统一由 `route_planner::geo` 提供。

use chrono::{FixedOffset, TimeZone};
use rand::rngs::StdRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Normal};

pub use route_planner::geo::{gcj02_to_bd09, met_per_deg_lng, wgs84_to_bd09, MET_PER_DEG_LAT};

/// 仅供地图显示的固定经度比例（约 39°N）。轨迹几何与距离计算必须使用按纬度校正的
/// [`met_per_deg_lng`] / [`LocalFrame`]，否则低纬度校区的东西向距离会偏差十余个百分点。
pub const MET_PER_DEG_LNG: f64 = 86_600.0;

/// 以 (c_lat, c_lng) 为原点的局部等距平面（米，x 向东、y 向北），经度比例按原点纬度校正。
#[derive(Clone, Copy, Debug)]
pub struct LocalFrame {
    pub c_lat: f64,
    pub c_lng: f64,
    m_lng: f64,
}

impl LocalFrame {
    pub fn new(c_lat: f64, c_lng: f64) -> Self {
        Self {
            c_lat,
            c_lng,
            m_lng: met_per_deg_lng(c_lat),
        }
    }
    /// 以点集质心为原点（点集为 `(lat, lng)`）。
    pub fn centered(points: &[(f64, f64)]) -> Self {
        let n = points.len().max(1) as f64;
        Self::new(
            points.iter().map(|q| q.0).sum::<f64>() / n,
            points.iter().map(|q| q.1).sum::<f64>() / n,
        )
    }
    /// (lat, lng) → (x, y) 米。
    pub fn to_xy(self, lat: f64, lng: f64) -> (f64, f64) {
        ((lng - self.c_lng) * self.m_lng, (lat - self.c_lat) * MET_PER_DEG_LAT)
    }
    /// (x, y) 米 → (lat, lng)。
    pub fn to_bd(self, x: f64, y: f64) -> (f64, f64) {
        (self.c_lat + y / MET_PER_DEG_LAT, self.c_lng + x / self.m_lng)
    }
}

/// 两个 (lat, lng) 点的局部平面距离平方（米²，经度比例取 `b` 的纬度）。
pub fn dist2_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dy = (a.0 - b.0) * MET_PER_DEG_LAT;
    let dx = (a.1 - b.1) * met_per_deg_lng(b.0);
    dx * dx + dy * dy
}

/// 按质心角度排序打卡点（`(lat, lng)`），形成自然环序。
pub fn radial_order(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    let cx = points.iter().map(|q| q.0).sum::<f64>() / n as f64;
    let cy = points.iter().map(|q| q.1).sum::<f64>() / n as f64;
    let mut ordered = points.to_vec();
    ordered.sort_by(|a, b| {
        (a.0 - cx)
            .atan2(a.1 - cy)
            .partial_cmp(&(b.0 - cx).atan2(b.1 - cy))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ordered
}

/// round(x, n)：按精确二进制值四舍五入。
pub fn round_to(x: f64, n: usize) -> f64 {
    let s = format!("{x:.n$}");
    s.parse().unwrap_or(x)
}

/// 可播种 RNG 封装。
pub struct Rng {
    inner: StdRng,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            inner: StdRng::seed_from_u64(seed),
        }
    }
    pub fn random(&mut self) -> f64 {
        rand::Rng::gen_range(&mut self.inner, 0.0..1.0)
    }
    pub fn uniform(&mut self, a: f64, b: f64) -> f64 {
        rand::Rng::gen_range(&mut self.inner, a..=b)
    }
    pub fn randint(&mut self, a: i64, b: i64) -> i64 {
        rand::Rng::gen_range(&mut self.inner, a..=b)
    }
    pub fn gauss(&mut self, mu: f64, sigma: f64) -> f64 {
        Normal::new(mu, sigma).unwrap().sample(&mut self.inner)
    }
    pub fn choice<T>(&mut self, items: &[T]) -> T
    where
        T: Copy,
    {
        items[rand::Rng::gen_range(&mut self.inner, 0..items.len())]
    }
    pub fn weighted<T>(&mut self, items: &[(T, u32)]) -> T
    where
        T: Copy,
    {
        let total: u32 = items.iter().map(|(_, w)| *w).sum();
        let mut u = self.uniform(0.0, total as f64);
        for (item, w) in items {
            u -= *w as f64;
            if u < 0.0 {
                return *item;
            }
        }
        items[items.len() - 1].0
    }
}

/// 打卡点 BD 系 (lat, lng) → 闭合平面路径 + 弧长表 + 中心。
pub type PointRing = (Vec<(f64, f64)>, Vec<f64>, (f64, f64));
///
/// 平面坐标为以质心为原点的 [`LocalFrame`]（经度比例按质心纬度校正）。
pub fn make_point_ring(bd_points: &[(f64, f64)]) -> PointRing {
    let n = bd_points.len();
    let frame = LocalFrame::centered(bd_points);
    let plane: Vec<(f64, f64)> = radial_order(bd_points)
        .iter()
        .map(|q| frame.to_xy(q.0, q.1))
        .collect();
    let samples = 18usize;
    let mut dense = Vec::with_capacity(n * samples);
    for i in 0..n {
        let p0 = plane[(i + n - 1) % n];
        let p1 = plane[i];
        let p2 = plane[(i + 1) % n];
        let p3 = plane[(i + 2) % n];
        for j in 0..samples {
            let t = j as f64 / samples as f64;
            let (t2, t3) = (t * t, t * t * t);
            let x = 0.5
                * ((2.0 * p1.0)
                    + (-p0.0 + p2.0) * t
                    + (2.0 * p0.0 - 5.0 * p1.0 + 4.0 * p2.0 - p3.0) * t2
                    + (-p0.0 + 3.0 * p1.0 - 3.0 * p2.0 + p3.0) * t3);
            let y = 0.5
                * ((2.0 * p1.1)
                    + (-p0.1 + p2.1) * t
                    + (2.0 * p0.1 - 5.0 * p1.1 + 4.0 * p2.1 - p3.1) * t2
                    + (-p0.1 + 3.0 * p1.1 - 3.0 * p2.1 + p3.1) * t3);
            dense.push((x, y));
        }
    }
    let mut arcs = vec![0.0f64];
    for i in 1..=dense.len() {
        let a = dense[i - 1];
        let b = dense[i % dense.len()];
        arcs.push(arcs[i - 1] + ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt());
    }
    (dense, arcs, (frame.c_lat, frame.c_lng))
}

/// 环线弧长 → 坐标（线性插值）。
///
/// `arcs[i]` 为 `dense[i % dense.len()]` 处的累计弧长：既支持末点显式闭合
/// （两者等长），也支持末段回绕到首点（`arcs` 多一项）。弧长按总长取模。
pub fn ring_point_at(dense: &[(f64, f64)], arcs: &[f64], s: f64) -> (f64, f64) {
    match (dense.first(), arcs.len()) {
        (None, _) | (_, 0) => return (0.0, 0.0),
        (Some(&p), 1) => return p,
        _ => {}
    }
    let total = *arcs.last().unwrap_or(&1.0);
    let s = s.rem_euclid(total);
    let mut lo = 0usize;
    let mut hi = arcs.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if arcs[mid] < s {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let i = lo.clamp(1, arcs.len() - 1);
    let a = dense[(i - 1) % dense.len()];
    let b = dense[i % dense.len()];
    let seg = arcs[i] - arcs[i - 1];
    let t = if seg > 0.0 {
        (s - arcs[i - 1]) / seg
    } else {
        0.0
    };
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

pub fn fmt_cst(ms: i64, format: &str) -> String {
    FixedOffset::east_opt(8 * 3600)
        .unwrap()
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

pub fn fmt_gain_time(ms: i64) -> String {
    fmt_cst(ms, "%Y-%m-%d %H:%M:%S")
}
