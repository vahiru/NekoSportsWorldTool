//! Local, datum-aware elevation raster. Rows run south to north; columns west to east.
//! Interpolation never extrapolates or silently fills missing DEM cells.

use route_planner::geo::{bd09_to_gcj02, wgs84_to_gcj02};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSystem {
    Wgs84,
    Gcj02,
    Bd09,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainGrid {
    pub source: String,
    /// e.g. EGM96, EGM2008, ellipsoidal:WGS84. No implicit datum conversion.
    pub vertical_datum: String,
    pub coordinate_system: CoordinateSystem,
    pub resolution_m: f64,
    pub west: f64,
    pub south: f64,
    pub longitude_step: f64,
    pub latitude_step: f64,
    pub heights_m: Vec<Vec<Option<f64>>>,
}

impl TerrainGrid {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("读取高程文件失败: {e}"))?;
        let grid: Self =
            serde_json::from_slice(&bytes).map_err(|e| format!("高程 JSON 格式错误: {e}"))?;
        grid.validate()?;
        Ok(grid)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.source.trim().is_empty() || self.vertical_datum.trim().is_empty() {
            return Err("高程必须声明 source 和 vertical_datum".into());
        }
        if ![self.resolution_m, self.longitude_step, self.latitude_step]
            .iter()
            .all(|v| v.is_finite() && *v > 0.0)
            || !self.west.is_finite()
            || !self.south.is_finite()
        {
            return Err("高程网格间距和分辨率必须为正有限数".into());
        }
        let cols = self.heights_m.first().map_or(0, Vec::len);
        if self.heights_m.len() < 2 || cols < 2 {
            return Err("高程网格至少需要 2×2 个节点".into());
        }
        if self.heights_m.iter().any(|r| {
            r.len() != cols
                || r.iter()
                    .flatten()
                    .any(|h| !h.is_finite() || !(-500.0..=9000.0).contains(h))
        }) {
            return Err("高程网格行长不一致或高度超出 -500..9000 米".into());
        }
        let east = self.west + (cols - 1) as f64 * self.longitude_step;
        let north = self.south + (self.heights_m.len() - 1) as f64 * self.latitude_step;
        if self.west < -180.0 || east > 180.0 || self.south < -90.0 || north > 90.0 {
            return Err("高程网格经纬度范围无效".into());
        }
        Ok(())
    }

    /// Query coordinates must use the raster's declared coordinate system.
    /// Call validate once before sampling; malformed public values still return errors.
    pub fn sample(&self, lat: f64, lon: f64) -> Result<f64, String> {
        let rows = self.heights_m.len();
        let cols = self.heights_m.first().map_or(0, Vec::len);
        if rows < 2 || cols < 2 || self.longitude_step <= 0.0 || self.latitude_step <= 0.0 {
            return Err("无效高程网格".into());
        }
        let x = (lon - self.west) / self.longitude_step;
        let y = (lat - self.south) / self.latitude_step;
        // Small allowance for floating point error exactly on the border.
        if !x.is_finite()
            || !y.is_finite()
            || x < -1e-9
            || y < -1e-9
            || x > (cols - 1) as f64 + 1e-9
            || y > (rows - 1) as f64 + 1e-9
        {
            return Err(format!("高程未覆盖坐标 ({lat:.7}, {lon:.7})"));
        }
        let x = x.clamp(0.0, (cols - 1) as f64);
        let y = y.clamp(0.0, (rows - 1) as f64);
        let i = (x.floor() as usize).min(cols - 2);
        let j = (y.floor() as usize).min(rows - 2);
        let (u, v) = (x - i as f64, y - j as f64);
        let mut height = 0.0;
        for (row, col, weight) in [
            (j, i, (1.0 - u) * (1.0 - v)),
            (j, i + 1, u * (1.0 - v)),
            (j + 1, i, (1.0 - u) * v),
            (j + 1, i + 1, u * v),
        ] {
            if weight > 1e-12 {
                let value = self
                    .heights_m
                    .get(row)
                    .and_then(|r| r.get(col))
                    .copied()
                    .flatten()
                    .filter(|h| h.is_finite())
                    .ok_or_else(|| format!("高程缺测：网格行 {row} 列 {col}"))?;
                height += value * weight;
            }
        }
        Ok(height)
    }
}

/// Conversion is explicit at the raster boundary. GCJ inverse uses iteration;
/// this is a map-coordinate conversion, not survey-grade geodesy.
pub fn convert_coordinate(
    lat: f64,
    lon: f64,
    from: CoordinateSystem,
    to: CoordinateSystem,
) -> (f64, f64) {
    use CoordinateSystem::*;
    if from == to {
        return (lat, lon);
    }
    let gcj = match from {
        Wgs84 => wgs84_to_gcj02(lat, lon),
        Gcj02 => (lat, lon),
        Bd09 => bd09_to_gcj02(lat, lon),
    };
    match to {
        Gcj02 => gcj,
        Bd09 => route_planner::geo::gcj02_to_bd09(gcj.0, gcj.1),
        Wgs84 => {
            if matches!(from, Wgs84) {
                return (lat, lon);
            }
            let mut guess = gcj;
            for _ in 0..10 {
                let projected = wgs84_to_gcj02(guess.0, guess.1);
                guess.0 += gcj.0 - projected.0;
                guess.1 += gcj.1 - projected.1;
            }
            guess
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn grid() -> TerrainGrid {
        TerrainGrid {
            source: "test plane".into(),
            vertical_datum: "EGM96".into(),
            coordinate_system: CoordinateSystem::Wgs84,
            resolution_m: 30.0,
            west: 110.0,
            south: 30.0,
            longitude_step: 0.001,
            latitude_step: 0.001,
            heights_m: vec![vec![Some(10.0), Some(20.0)], vec![Some(30.0), Some(40.0)]],
        }
    }
    #[test]
    fn bilinear_edges_missing_and_coverage() {
        let mut g = grid();
        g.validate().unwrap();
        assert!((g.sample(30.0005, 110.0005).unwrap() - 25.0).abs() < 1e-8);
        assert!((g.sample(30.001, 110.001).unwrap() - 40.0).abs() < 1e-8);
        assert!(g.sample(30.002, 110.0).is_err());
        g.heights_m[1][1] = None;
        assert!(g.sample(30.0005, 110.0005).is_err());
        assert_eq!(g.sample(30.0, 110.0).unwrap(), 10.0);
    }
    #[test]
    fn map_coordinate_round_trip() {
        let bd = convert_coordinate(30.0, 110.0, CoordinateSystem::Wgs84, CoordinateSystem::Bd09);
        let wgs = convert_coordinate(bd.0, bd.1, CoordinateSystem::Bd09, CoordinateSystem::Wgs84);
        assert!((wgs.0 - 30.0).abs() < 2e-6 && (wgs.1 - 110.0).abs() < 2e-6);
    }
}
