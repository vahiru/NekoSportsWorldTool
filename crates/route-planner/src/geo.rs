//! 经纬度几何与坐标基准换算：路网层与轨迹层共用的唯一实现。
//!
//! 约定：`(lat, lng)` 元组均为「纬度在前」；`Coord` 为 `(lon, lat)` 字段。

use crate::graph::Coord;

/// 纬度每度对应米数（近似常数）。
pub const MET_PER_DEG_LAT: f64 = 111_132.0;
/// 赤道处经度每度对应米数；其它纬度乘以 `cos(lat)`，见 [`met_per_deg_lng`]。
pub const MET_PER_DEG_LNG_EQ: f64 = 111_320.0;

/// 给定纬度处经度每度对应米数（高纬度下限 0.2 防止退化）。
pub fn met_per_deg_lng(lat: f64) -> f64 {
    MET_PER_DEG_LNG_EQ * lat.to_radians().cos().abs().max(0.2)
}

/// 球面大圆距离（米，haversine）。
pub fn haversine_m(a: Coord, b: Coord) -> f64 {
    use geo::HaversineDistance;
    geo::Point::new(a.lon, a.lat).haversine_distance(&geo::Point::new(b.lon, b.lat))
}

/// 球面大圆距离（米），参数为 `(lat, lng)`。
pub fn haversine_latlng(a: (f64, f64), b: (f64, f64)) -> f64 {
    haversine_m(Coord::new(a.1, a.0), Coord::new(b.1, b.0))
}

// ── 坐标基准转换：WGS84 ↔ GCJ-02 ↔ BD-09 ─────────────────────────
// 打卡点来自服务端为 BD-09；OSM 路网为 WGS84。

const X_PI: f64 = std::f64::consts::PI * 3000.0 / 180.0;
const WGS_A: f64 = 6378245.0;
#[allow(clippy::excessive_precision)]
const WGS_EE: f64 = 0.00669342162296594323;

fn out_of_china(lat: f64, lng: f64) -> bool {
    !(72.004..=137.8347).contains(&lng) || !(0.8293..=55.8271).contains(&lat)
}

fn transform_lat(x: f64, y: f64) -> f64 {
    let pi = std::f64::consts::PI;
    let mut ret = -100.0 + 2.0 * x + 3.0 * y + 0.2 * y * y + 0.1 * x * y + 0.2 * x.abs().sqrt();
    ret += (20.0 * (6.0 * x * pi).sin() + 20.0 * (2.0 * x * pi).sin()) * 2.0 / 3.0;
    ret += (20.0 * (y * pi).sin() + 40.0 * (y / 3.0 * pi).sin()) * 2.0 / 3.0;
    ret += (160.0 * (y / 12.0 * pi).sin() + 320.0 * (y * pi / 30.0).sin()) * 2.0 / 3.0;
    ret
}

fn transform_lng(x: f64, y: f64) -> f64 {
    let pi = std::f64::consts::PI;
    let mut ret = 300.0 + x + 2.0 * y + 0.1 * x * x + 0.1 * x * y + 0.1 * x.abs().sqrt();
    ret += (20.0 * (6.0 * x * pi).sin() + 20.0 * (2.0 * x * pi).sin()) * 2.0 / 3.0;
    ret += (20.0 * (x * pi).sin() + 40.0 * (x / 3.0 * pi).sin()) * 2.0 / 3.0;
    ret += (150.0 * (x / 12.0 * pi).sin() + 300.0 * (x / 30.0 * pi).sin()) * 2.0 / 3.0;
    ret
}

/// WGS84 → GCJ-02（火星坐标，境内偏移）。
pub fn wgs84_to_gcj02(lat: f64, lng: f64) -> (f64, f64) {
    if out_of_china(lat, lng) {
        return (lat, lng);
    }
    let pi = std::f64::consts::PI;
    let mut dlat = transform_lat(lng - 105.0, lat - 35.0);
    let mut dlng = transform_lng(lng - 105.0, lat - 35.0);
    let radlat = lat / 180.0 * pi;
    let mut magic = radlat.sin();
    magic = 1.0 - WGS_EE * magic * magic;
    let sqrtmagic = magic.sqrt();
    dlat = (dlat * 180.0) / ((WGS_A * (1.0 - WGS_EE)) / (magic * sqrtmagic) * pi);
    dlng = (dlng * 180.0) / (WGS_A / sqrtmagic * radlat.cos() * pi);
    (lat + dlat, lng + dlng)
}

/// GCJ-02 → BD-09（[`bd09_to_gcj02`] 的逆变换）。
pub fn gcj02_to_bd09(gcj_lat: f64, gcj_lng: f64) -> (f64, f64) {
    let z = (gcj_lng * gcj_lng + gcj_lat * gcj_lat).sqrt() + 0.00002 * (gcj_lat * X_PI).sin();
    let theta = gcj_lat.atan2(gcj_lng) + 0.000003 * (gcj_lng * X_PI).cos();
    (z * theta.sin() + 0.006, z * theta.cos() + 0.0065)
}

/// 百度 BD-09 → 高德 GCJ-02。
pub fn bd09_to_gcj02(bd_lat: f64, bd_lng: f64) -> (f64, f64) {
    let x = bd_lng - 0.0065;
    let y = bd_lat - 0.006;
    let z = (x * x + y * y).sqrt() - 0.00002 * (y * X_PI).sin();
    let theta = y.atan2(x) - 0.000003 * (x * X_PI).cos();
    (z * theta.sin(), z * theta.cos())
}

/// WGS84 → BD-09（OSM 路网对齐打卡点用）。
pub fn wgs84_to_bd09(lat: f64, lng: f64) -> (f64, f64) {
    let (g_lat, g_lng) = wgs84_to_gcj02(lat, lng);
    gcj02_to_bd09(g_lat, g_lng)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_scale_matches_haversine_at_several_latitudes() {
        for lat in [0.0, 23.0, 30.0, 39.9, 50.0] {
            let a = Coord::new(110.0, lat);
            let b = Coord::new(110.01, lat);
            let flat = 0.01 * met_per_deg_lng(lat);
            let sphere = haversine_m(a, b);
            assert!(
                (flat / sphere - 1.0).abs() < 0.002,
                "lat={lat}: {flat} vs {sphere}"
            );
        }
    }
}
