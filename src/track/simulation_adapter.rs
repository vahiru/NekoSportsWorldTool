//! Terrain pipeline → existing Track. Internal positions are BD09, speed is km/h
//! for `speed` and m/s for `avgSpeed`. Never apply legacy protocol post-fixes here.

use super::geom::{fmt_gain_time, round_to};
use super::model::{GenPoint, Track};
use crate::simulation::{
    simulate, RouteCoordinate, RouteInput, SensorSample, SimulationConfig, SimulationOutput,
};
use crate::terrain::{convert_coordinate, CoordinateSystem, TerrainGrid};
use route_planner::geo::haversine_latlng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerrainSimulationSettings {
    pub enabled: bool,
    pub terrain_path: String,
    pub config_path: String,
}

pub struct TerrainRunContext {
    pub terrain: TerrainGrid,
    pub config: SimulationConfig,
}

#[derive(Serialize)]
pub struct TimestampedSensor<'a> {
    pub timestamp_ms: i64,
    #[serde(flatten)]
    pub observation: &'a SensorSample,
}

/// Absolute, dynamic timestamps without cloning the high-rate sensor series.
pub fn absolute_sensors(
    track: &Track,
) -> impl DoubleEndedIterator<Item = Result<TimestampedSensor<'_>, String>> {
    track
        .simulation
        .iter()
        .flat_map(|output| output.sensors.iter())
        .map(move |observation| {
            let elapsed = observation.time_s() * 1000.0;
            if !elapsed.is_finite() || elapsed < 0.0 || elapsed >= i64::MAX as f64 {
                return Err("传感器时间超出协议范围".into());
            }
            let timestamp_ms = track
                .startTime
                .checked_add(elapsed.round() as i64)
                .ok_or("传感器绝对时间溢出")?;
            Ok(TimestampedSensor {
                timestamp_ms,
                observation,
            })
        })
}

impl TerrainSimulationSettings {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn load(&self) -> Result<Option<Arc<TerrainRunContext>>, String> {
        if !self.enabled {
            return Ok(None);
        }
        if self.terrain_path.trim().is_empty() {
            return Err("山地仿真需要配置高程网格文件".into());
        }
        let terrain = TerrainGrid::load(&self.terrain_path)?;
        let config = if self.config_path.trim().is_empty() {
            SimulationConfig::default()
        } else {
            let bytes = std::fs::read(&self.config_path)
                .map_err(|e| format!("读取山地仿真配置失败: {e}"))?;
            serde_json::from_slice::<SimulationConfig>(&bytes)
                .map_err(|e| format!("山地仿真配置无效: {e}"))?
        };
        config.validate()?;
        Ok(Some(Arc::new(TerrainRunContext { terrain, config })))
    }

    pub fn validate_altitude(
        &self,
        single: Option<f64>,
        range: Option<super::altitude::AltitudeRange>,
    ) -> Result<(), String> {
        if self.enabled && (single.is_some() || range.is_some()) {
            return Err("山地仿真不能与手动海拔同时启用；请清空手动海拔或关闭山地模式".into());
        }
        Ok(())
    }
}

/// Shortest final piece; stays within the 0.1m distance tolerance in `generate`.
const MIN_TAIL_M: f64 = 0.05;

/// Preserve corners and repeat a BD09 ring up to the requested horizontal length.
/// The final segment is clipped geometrically; no distance redistribution occurs.
pub fn route_for_distance(ring: &[(f64, f64)], distance_m: f64) -> Result<RouteInput, String> {
    if !distance_m.is_finite() || distance_m <= 0.0 || ring.len() < 2 {
        return Err("山地路线需要有效距离和至少两个不同节点".into());
    }
    let mut clean = Vec::new();
    for &p in ring {
        crate::location::Coordinate::new(p.0, p.1, 0.0)?;
        if clean
            .last()
            .is_none_or(|&prev| haversine_latlng(prev, p) > 0.01)
        {
            clean.push(p);
        }
    }
    if clean.len() < 2 {
        return Err("山地路线退化为单个位置".into());
    }
    if haversine_latlng(clean[0], *clean.last().unwrap()) > 0.01 {
        clean.push(clean[0]);
    } else {
        *clean.last_mut().unwrap() = clean[0];
    }
    let mut points = vec![RouteCoordinate {
        latitude: clean[0].0,
        longitude: clean[0].1,
    }];
    let mut remaining = distance_m;
    let mut index = 0;
    while remaining > 1e-7 {
        if points.len() >= 100_000 {
            return Err("山地路线展开超过 100000 节点，请缩短距离或简化路网".into());
        }
        let (a, b) = (clean[index], clean[index + 1]);
        let len = haversine_latlng(a, b);
        // A sub-centimetre final piece would trip the 1mm segment check or the
        // 1ms timestamp resolution; overshoot to MIN_TAIL_M instead.
        let k = (remaining.max(MIN_TAIL_M) / len).min(1.0);
        let p = RouteCoordinate {
            latitude: a.0 + (b.0 - a.0) * k,
            longitude: a.1 + (b.1 - a.1) * k,
        };
        points.push(p);
        remaining = (remaining - len).max(0.0);
        index = (index + 1) % (clean.len() - 1);
    }
    let first = points[0];
    let last = points.last().unwrap();
    let closed = (first.latitude - last.latitude).abs() < 1e-10
        && (first.longitude - last.longitude).abs() < 1e-10;
    Ok(RouteInput {
        coordinate_system: CoordinateSystem::Bd09,
        closed,
        points,
    })
}

pub fn generate(
    ring: &[(f64, f64)],
    distance_m: f64,
    duration_s: i64,
    seed: u64,
    start_ms: i64,
    context: &TerrainRunContext,
) -> Result<Track, String> {
    if duration_s <= 0 {
        return Err("山地仿真时长必须为正数".into());
    }
    let route = route_for_distance(ring, distance_m)?;
    let mut config = context.config.clone();
    // Run parameters own the one shared time window and seed. Do not use the
    // flat-ground effective_duration clamp or a config-file target for submissions.
    config.target_duration_s = Some(duration_s as f64);
    config.seed = seed;
    config.sensors = true;
    let simulation = simulate(&route, &context.terrain, &config)?;
    if (simulation.distance_m - distance_m).abs() > 0.1 {
        return Err("山地路线几何长度与目标距离偏差超过 0.1m".into());
    }
    from_simulation(simulation, start_ms)
}

/// Full observations remain attached to Track. The known server schema has no
/// pressure/accelerometer sequence fields; do not invent wire keys for them.
pub fn from_simulation(output: SimulationOutput, start_ms: i64) -> Result<Track, String> {
    let report = output.validate();
    if !report.passed {
        return Err(format!("不能适配无效仿真: {:?}", report.issues));
    }
    if (output.duration_s - output.duration_s.round()).abs() > 1e-6 {
        return Err("提交协议要求整数秒总时长，请使用整数 target_duration_s 重新仿真".into());
    }
    let heights = observed_heights(&output)?;
    let mut locations = Vec::with_capacity(output.motion.len());
    for (i, (motion, height)) in output.motion.iter().zip(heights).enumerate() {
        let elapsed = (motion.time_s * 1000.0).round();
        if elapsed < 0.0 || elapsed >= i64::MAX as f64 {
            return Err("仿真时间超出协议范围".into());
        }
        let timestamp = start_ms
            .checked_add(elapsed as i64)
            .ok_or("仿真绝对时间溢出")?;
        if locations
            .last()
            .is_some_and(|p: &GenPoint| p.gainTimeMs >= timestamp)
        {
            return Err("相邻山地节点间隔不足 1ms，无法无损映射协议时间戳".into());
        }
        let time = elapsed as i64 / 1000;
        let (lat, lon) = convert_coordinate(
            motion.latitude,
            motion.longitude,
            output.coordinate_system,
            CoordinateSystem::Bd09,
        );
        // The last node keeps its incoming direction instead of a zero-length 0°.
        let (from, to) = match output.motion.get(i + 1) {
            Some(next) => (motion, next),
            None => (&output.motion[i - 1], motion),
        };
        let heading = ((to.longitude - from.longitude) * from.latitude.to_radians().cos())
            .atan2(to.latitude - from.latitude)
            .to_degrees()
            .rem_euclid(360.0);
        let gain_time = fmt_gain_time(timestamp);
        if gain_time.is_empty() {
            return Err("仿真时间无法转换为协议日期".into());
        }
        locations.push(GenPoint {
            id: i as i64 + 1,
            flag: start_ms,
            lat: -1.0,
            lng: -1.0,
            gLat: lat,
            gLng: lon,
            speed: round_to(motion.speed_mps * 3.6, 4),
            avgSpeed: round_to(motion.speed_mps, 4),
            radius: 3.0,
            accuracy: 3.0,
            ptype: if i == output.motion.len() - 1 { 6 } else { 0 },
            locType: 1,
            hasAltitude: true,
            totalTime: time,
            totalDis: round_to(motion.distance_m, 4),
            validDis: round_to(motion.distance_m, 4),
            validTime: time,
            steps: motion.cumulative_steps.round() as i64,
            stepDistance: round_to(motion.step_length_m, 4),
            gainTime: gain_time,
            gainTimeMs: timestamp,
            queueNum: 0,
            coorType: "gcj02".into(),
            bdA: round_to(height, 2),
            bdD: round_to(heading, 2),
            bdS: round_to(motion.speed_mps, 4),
            bdG: 1,
            count: 0,
            dtr: 0.0,
            state: 1,
            locationId: String::new(),
        });
    }
    let mut track = super::synth::assemble(
        locations,
        output.duration_s,
        output.distance_m,
        output.motion.last().unwrap().cumulative_steps,
        start_ms,
    );
    track.simulation = Some(Arc::new(output));
    super::stats::rebuild(&mut track);
    track.validate_consistency()?;
    Ok(track)
}

fn observed_heights(output: &SimulationOutput) -> Result<Vec<f64>, String> {
    let gnss: Vec<_> = output
        .sensors
        .iter()
        .filter_map(|sample| match sample {
            SensorSample::GnssHeight {
                time_s,
                observed_height_m,
            } => Some((*time_s, *observed_height_m)),
            _ => None,
        })
        .collect();
    if gnss.is_empty() {
        return Err("协议适配需要 GNSS 观测高度，请启用 sensors".into());
    }
    // Interpolate observation residuals, not absolute height, so route corners
    // keep their ground profile. Clamp the last sub-second to the last GNSS fix.
    let residuals: Vec<_> = gnss
        .iter()
        .map(|&(t, h)| (t, h - output.height_at_time(t)))
        .collect();
    let mut cursor = 0;
    Ok(output
        .motion
        .iter()
        .map(|p| {
            while cursor + 1 < residuals.len() && residuals[cursor + 1].0 <= p.time_s {
                cursor += 1;
            }
            let (t, r) = residuals[cursor];
            let residual = if let Some(&(next_t, next_r)) = residuals.get(cursor + 1) {
                r + (next_r - r) * ((p.time_s - t) / (next_t - t)).clamp(0.0, 1.0)
            } else {
                r
            };
            p.ground_height_m + residual
        })
        .collect())
}

pub(super) fn validate_track(track: &Track) -> Result<(), String> {
    let Some(output) = &track.simulation else {
        return Ok(());
    };
    if !output.validate().passed || track.locations.len() != output.motion.len() {
        return Err("适配后的仿真数据无效或节点数改变".into());
    }
    absolute_sensors(track).next_back().transpose()?;
    let heights = observed_heights(output)?;
    for ((p, m), h) in track.locations.iter().zip(&output.motion).zip(heights) {
        let expected_coordinate = convert_coordinate(
            m.latitude,
            m.longitude,
            output.coordinate_system,
            CoordinateSystem::Bd09,
        );
        let expected_ms = track
            .startTime
            .checked_add((m.time_s * 1000.0).round() as i64)
            .ok_or("时间溢出")?;
        if ![
            p.speed,
            p.avgSpeed,
            p.bdA,
            p.totalDis,
            p.stepDistance,
            p.gLat,
            p.gLng,
        ]
        .iter()
        .all(|v| v.is_finite())
            || p.gainTimeMs != expected_ms
            || p.totalTime != (m.time_s * 1000.0).round() as i64 / 1000
            || (p.totalDis - m.distance_m).abs() > 0.000051
            || p.steps != m.cumulative_steps.round() as i64
            || (p.speed - m.speed_mps * 3.6).abs() > 0.000051
            || (p.avgSpeed - m.speed_mps).abs() > 0.000051
            || (p.bdA - h).abs() > 0.00501
            || (p.gLat - expected_coordinate.0).abs() > 1e-9
            || (p.gLng - expected_coordinate.1).abs() > 1e-9
            || p.gainTime != fmt_gain_time(expected_ms)
            || !p.hasAltitude
            || p.validDis != p.totalDis
            || p.validTime != p.totalTime
        {
            return Err("协议节点与仿真观测/运动状态不一致，禁止后处理改写".into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "simulation_adapter_tests.rs"]
pub(crate) mod tests;
