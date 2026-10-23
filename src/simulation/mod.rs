//! Local terrain-aware simulation, independent of API/protocol post-processing.
//! Horizontal distance is the independent variable; observations never drive physics.

mod sensors;
mod validation;

use crate::terrain::{convert_coordinate, CoordinateSystem, TerrainGrid};
use route_planner::geo::haversine_latlng;
pub use sensors::{pressure_hpa, SensorSample};
use serde::{Deserialize, Serialize};
pub use validation::{Issue, ValidationReport};

const MAX_POINTS: usize = 200_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteInput {
    pub coordinate_system: CoordinateSystem,
    /// A closed route must explicitly repeat its first coordinate at the end.
    #[serde(default)]
    pub closed: bool,
    pub points: Vec<RouteCoordinate>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteCoordinate {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimulationConfig {
    pub spacing_m: f64,
    pub smoothing_radius_m: f64,
    pub spike_threshold_m: f64,
    pub base_speed_mps: f64,
    pub max_speed_mps: f64,
    pub max_climb_mps: f64,
    pub max_descent_mps: f64,
    pub max_acceleration_mps2: f64,
    pub max_braking_mps2: f64,
    pub max_lateral_acceleration_mps2: f64,
    pub target_duration_s: Option<f64>,
    pub seed: u64,
    /// Optional multi-rate sensors; high-rate accelerometer output may be large.
    pub sensors: bool,
    pub acceleration_hz: u32,
    pub gnss_height_sigma_m: f64,
    /// Reference atmosphere: pressure at reference_height_m, not necessarily sea level.
    pub reference_pressure_hpa: f64,
    pub reference_height_m: f64,
    pub reference_temperature_k: f64,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            spacing_m: 5.0,
            smoothing_radius_m: 15.0,
            spike_threshold_m: 8.0,
            base_speed_mps: 2.5,
            max_speed_mps: 5.0,
            max_climb_mps: 0.3,
            max_descent_mps: 0.6,
            max_acceleration_mps2: 0.8,
            max_braking_mps2: 1.2,
            max_lateral_acceleration_mps2: 2.5,
            target_duration_s: None,
            seed: 1,
            sensors: false,
            acceleration_hz: 50,
            gnss_height_sigma_m: 0.5,
            reference_pressure_hpa: 1013.25,
            reference_height_m: 0.0,
            reference_temperature_k: 288.15,
        }
    }
}

impl SimulationConfig {
    pub fn validate(&self) -> Result<(), String> {
        if ![
            self.spacing_m,
            self.spike_threshold_m,
            self.base_speed_mps,
            self.max_speed_mps,
            self.max_climb_mps,
            self.max_descent_mps,
            self.max_acceleration_mps2,
            self.max_braking_mps2,
            self.max_lateral_acceleration_mps2,
            self.reference_pressure_hpa,
            self.reference_temperature_k,
        ]
        .iter()
        .all(|x| x.is_finite() && *x > 0.0)
            || !self.reference_height_m.is_finite()
            || !self.smoothing_radius_m.is_finite()
            || self.smoothing_radius_m < 0.0
            || !self.gnss_height_sigma_m.is_finite()
            || self.gnss_height_sigma_m < 0.0
            || self
                .target_duration_s
                .is_some_and(|t| !t.is_finite() || t <= 0.0)
            || !(1..=200).contains(&self.acceleration_hz)
            || self.base_speed_mps > self.max_speed_mps
        {
            return Err("仿真参数无效：检查正数、有限数、基准速度与采样率 (1..200 Hz)".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MotionPoint {
    pub latitude: f64,
    pub longitude: f64,
    pub distance_m: f64,
    pub time_s: f64,
    pub raw_height_m: f64,
    pub ground_height_m: f64,
    pub corrected_spike: bool,
    /// Incoming interval slope dh / horizontal ds; zero on the first point.
    pub grade: f64,
    pub speed_mps: f64,
    pub cadence_spm: f64,
    pub step_length_m: f64,
    pub cumulative_steps: f64,
}

#[derive(Debug, Serialize)]
pub struct SimulationOutput {
    pub schema_version: u32,
    pub coordinate_system: CoordinateSystem,
    pub terrain_source: String,
    pub vertical_datum: String,
    pub terrain_resolution_m: f64,
    pub config: SimulationConfig,
    pub distance_m: f64,
    pub duration_s: f64,
    pub ascent_m: f64,
    pub descent_m: f64,
    pub fastest_duration_s: f64,
    pub warnings: Vec<String>,
    pub motion: Vec<MotionPoint>,
    pub sensors: Vec<SensorSample>,
    pub validation: ValidationReport,
}

impl SimulationOutput {
    pub(crate) fn height_at_time(&self, time_s: f64) -> f64 {
        sensors::state_at(&self.motion, time_s).height
    }
    pub(crate) fn counters_at_time(&self, time_s: f64) -> (f64, f64) {
        let s = sensors::state_at(&self.motion, time_s);
        (s.distance, s.steps)
    }
    pub(crate) fn counters_at_distance(&self, distance_m: f64) -> (f64, f64) {
        let i = self
            .motion
            .partition_point(|p| p.distance_m < distance_m)
            .clamp(1, self.motion.len() - 1);
        let (a, b) = (&self.motion[i - 1], &self.motion[i]);
        let ds = (distance_m - a.distance_m).clamp(0.0, b.distance_m - a.distance_m);
        let acceleration = (b.speed_mps - a.speed_mps) / (b.time_s - a.time_s);
        let speed = (a.speed_mps * a.speed_mps + 2.0 * acceleration * ds)
            .max(0.0)
            .sqrt();
        let elapsed = 2.0 * ds / (a.speed_mps + speed);
        let time = (a.time_s + elapsed).min(b.time_s);
        (time, sensors::state_at(&self.motion, time).steps)
    }
}

/// Simulate an explicit route without reordering, shortcutting or adding laps.
/// Every original road corner remains a node, even when shorter than spacing_m.
pub fn simulate(
    route: &RouteInput,
    terrain: &TerrainGrid,
    config: &SimulationConfig,
) -> Result<SimulationOutput, String> {
    terrain.validate()?;
    config.validate()?;
    let mut motion = sample_route(route, terrain, config.spacing_m)?;
    filter_heights(&mut motion, route.closed, config)?;
    let mut warnings = vec![format!(
        "高程来源 {}，原始分辨率 {:.1}m；插值不提高原始精度；桥梁、台阶需实测校准",
        terrain.source, terrain.resolution_m
    )];
    if motion.iter().any(|p| p.corrected_spike) {
        warnings.push("已修正孤立高程尖峰，请结合实测复核".into());
    }
    if route.closed {
        warnings.push("闭环接缝按空间周期平滑；起终速度分别建模为单次运动的边界".into());
    }
    if terrain
        .vertical_datum
        .to_ascii_lowercase()
        .contains("ellipsoid")
    {
        warnings.push("椭球高未转换为正高；气压参考高度必须使用相同高程基准".into());
    }
    let (speeds, fastest) = solve_speeds(&motion, config)?;
    apply_motion(&mut motion, &speeds);
    let mut ascent = 0.0;
    let mut descent = 0.0;
    for pair in motion.windows(2) {
        let dh = pair[1].ground_height_m - pair[0].ground_height_m;
        ascent += dh.max(0.0);
        descent += (-dh).max(0.0);
    }
    let last = motion.last().unwrap();
    let mut output = SimulationOutput {
        schema_version: 1,
        coordinate_system: route.coordinate_system,
        terrain_source: terrain.source.clone(),
        vertical_datum: terrain.vertical_datum.clone(),
        terrain_resolution_m: terrain.resolution_m,
        config: config.clone(),
        distance_m: last.distance_m,
        duration_s: last.time_s,
        ascent_m: ascent,
        descent_m: descent,
        fastest_duration_s: fastest,
        warnings,
        motion,
        sensors: Vec::new(),
        validation: ValidationReport::default(),
    };
    if config.sensors {
        output.warnings.push(
            "加速度为理想前/左/上坐标系的示意步态波形，包含重力；尚未进行个体生物力学校准".into(),
        );
        let max_step_hz = output
            .motion
            .iter()
            .map(|p| p.cadence_spm / 60.0)
            .fold(0.0, f64::max);
        if (config.acceleration_hz as f64) < 4.0 * max_step_hz {
            output
                .warnings
                .push("加速度采样率不足以解析步态二次谐波，可能发生混叠；建议使用 50Hz".into());
        }
        output.sensors = sensors::generate(&output.motion, config)?;
    }
    output.validation = output.validate();
    if !output.validation.passed {
        return Err(format!(
            "仿真一致性校验失败: {:?}",
            output.validation.issues
        ));
    }
    Ok(output)
}

fn sample_route(
    route: &RouteInput,
    terrain: &TerrainGrid,
    spacing: f64,
) -> Result<Vec<MotionPoint>, String> {
    if route.points.len() < 2 || route.points.len() > MAX_POINTS {
        return Err("路线点数必须为 2..200000".into());
    }
    for p in &route.points {
        if !p.latitude.is_finite()
            || !p.longitude.is_finite()
            || p.latitude.abs() > 85.0
            || p.longitude.abs() > 180.0
        {
            return Err("路线经纬度无效（纬度支持 -85..85）".into());
        }
    }
    if route.closed {
        let first = route.points[0];
        let last = route.points.last().unwrap();
        if (first.latitude - last.latitude).abs() > 1e-10
            || (first.longitude - last.longitude).abs() > 1e-10
        {
            return Err("闭环路线必须在末尾重复首点坐标".into());
        }
    }
    let mut result = Vec::new();
    let mut s = 0.0;
    for (index, pair) in route.points.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        if (a.longitude - b.longitude).abs() > 180.0 {
            return Err("暂不支持跨越日期变更线的路线".into());
        }
        let length = haversine_latlng((a.latitude, a.longitude), (b.latitude, b.longitude));
        if length < 0.001 {
            return Err(format!("路线第 {} 段包含重复点或长度小于 1mm", index + 1));
        }
        let n = (length / spacing).ceil().max(1.0) as usize;
        if n > MAX_POINTS || result.len().saturating_add(n + 1) > MAX_POINTS {
            return Err("空间采样点超过 200000，请增大 spacing_m 或缩短路线".into());
        }
        for j in 0..=n {
            if index > 0 && j == 0 {
                continue;
            }
            let k = j as f64 / n as f64;
            let lat = a.latitude + (b.latitude - a.latitude) * k;
            let lon = a.longitude + (b.longitude - a.longitude) * k;
            let query =
                convert_coordinate(lat, lon, route.coordinate_system, terrain.coordinate_system);
            let raw = terrain.sample(query.0, query.1).map_err(|e| {
                format!("路线第 {} 段，累计 {:.1}m: {e}", index + 1, s + length * k)
            })?;
            result.push(MotionPoint {
                latitude: lat,
                longitude: lon,
                distance_m: s + length * k,
                time_s: 0.0,
                raw_height_m: raw,
                ground_height_m: raw,
                corrected_spike: false,
                grade: 0.0,
                speed_mps: 0.0,
                cadence_spm: 0.0,
                step_length_m: 0.0,
                cumulative_steps: 0.0,
            });
        }
        s += length;
    }
    Ok(result)
}

fn filter_heights(
    points: &mut [MotionPoint],
    closed: bool,
    config: &SimulationConfig,
) -> Result<(), String> {
    let n = points.len();
    let length = points[n - 1].distance_m;
    let mut heights: Vec<f64> = points.iter().map(|p| p.raw_height_m).collect();
    // Only isolated opposing jumps are replaced, using distance-aware interpolation.
    for i in 1..n - 1 {
        let a = points[i].raw_height_m - points[i - 1].raw_height_m;
        let b = points[i + 1].raw_height_m - points[i].raw_height_m;
        if a * b < 0.0 && a.abs().min(b.abs()) > config.spike_threshold_m {
            let k = (points[i].distance_m - points[i - 1].distance_m)
                / (points[i + 1].distance_m - points[i - 1].distance_m);
            heights[i] = points[i - 1].raw_height_m
                + (points[i + 1].raw_height_m - points[i - 1].raw_height_m) * k;
            points[i].corrected_spike = true;
        }
    }
    let radius = config
        .smoothing_radius_m
        .min(if closed { length / 2.0 } else { length });
    if radius > 0.0 {
        // Periodic extension makes the seam use the same neighborhood as any other point.
        // Arc-length weights prevent dense road corners dominating the spatial filter.
        let unique = if closed { n - 1 } else { n };
        let mut samples = Vec::new();
        let offsets: &[f64] = if closed {
            &[-length, 0.0, length]
        } else {
            &[0.0]
        };
        for offset in offsets {
            for i in 0..unique {
                let prev = if i > 0 {
                    points[i - 1].distance_m
                } else if closed {
                    points[n - 2].distance_m - length
                } else {
                    0.0
                };
                let next = if i + 1 < n {
                    points[i + 1].distance_m
                } else {
                    length
                };
                samples.push((
                    points[i].distance_m + offset,
                    heights[i],
                    (next - prev) / 2.0,
                ));
            }
        }
        let mut left = 0;
        let mut right = 0;
        let mut visits = 0usize;
        for p in points.iter_mut() {
            while left < samples.len() && samples[left].0 < p.distance_m - radius {
                left += 1;
            }
            right = right.max(left);
            while right < samples.len() && samples[right].0 <= p.distance_m + radius {
                right += 1;
            }
            visits = visits.saturating_add(right - left);
            if visits > 20_000_000 {
                return Err("空间平滑计算量过大，请减小 smoothing_radius_m 或减少采样点".into());
            }
            let (mut sum, mut weight) = (0.0, 0.0);
            for &(s, h, w) in &samples[left..right] {
                let g = (-0.5 * ((s - p.distance_m) / (radius / 2.0)).powi(2)).exp() * w;
                sum += h * g;
                weight += g;
            }
            if weight > 0.0 {
                p.ground_height_m = sum / weight;
            }
        }
    } else {
        for (p, h) in points.iter_mut().zip(heights) {
            p.ground_height_m = h;
        }
    }
    if closed {
        points[n - 1].ground_height_m = points[0].ground_height_m;
    }
    for i in 1..n {
        points[i].grade = (points[i].ground_height_m - points[i - 1].ground_height_m)
            / (points[i].distance_m - points[i - 1].distance_m);
    }
    Ok(())
}

fn solve_speeds(points: &[MotionPoint], c: &SimulationConfig) -> Result<(Vec<f64>, f64), String> {
    let n = points.len();
    let mut caps = vec![c.max_speed_mps; n];
    for i in 1..n {
        let g = points[i].grade;
        let vertical_cap = if g > 0.0 {
            c.max_climb_mps / g
        } else if g < 0.0 {
            c.max_descent_mps / -g
        } else {
            c.max_speed_mps
        };
        caps[i - 1] = caps[i - 1].min(vertical_cap);
        caps[i] = caps[i].min(vertical_cap);
    }
    for i in 1..n - 1 {
        let a = &points[i - 1];
        let b = &points[i];
        let d = &points[i + 1];
        let scale = b.latitude.to_radians().cos();
        let u = ((b.longitude - a.longitude) * scale, b.latitude - a.latitude);
        let v = ((d.longitude - b.longitude) * scale, d.latitude - b.latitude);
        let angle = ((u.0 * v.0 + u.1 * v.1) / (u.0.hypot(u.1) * v.0.hypot(v.1)))
            .clamp(-1.0, 1.0)
            .acos();
        if angle > 1e-6 {
            let ds = (b.distance_m - a.distance_m).min(d.distance_m - b.distance_m);
            caps[i] = caps[i].min((c.max_lateral_acceleration_mps2 * ds / angle).sqrt());
        }
    }
    let constrain = |mut speeds: Vec<f64>| {
        for i in 1..n {
            let ds = points[i].distance_m - points[i - 1].distance_m;
            speeds[i] =
                speeds[i].min((speeds[i - 1].powi(2) + 2.0 * c.max_acceleration_mps2 * ds).sqrt());
        }
        for i in (0..n - 1).rev() {
            let ds = points[i + 1].distance_m - points[i].distance_m;
            speeds[i] =
                speeds[i].min((speeds[i + 1].powi(2) + 2.0 * c.max_braking_mps2 * ds).sqrt());
        }
        speeds
    };
    let duration = |v: &[f64]| -> f64 {
        (1..n)
            .map(|i| 2.0 * (points[i].distance_m - points[i - 1].distance_m) / (v[i] + v[i - 1]))
            .sum()
    };
    let fastest = duration(&constrain(caps.clone()));
    let natural: Vec<f64> = points
        .iter()
        .map(|p| {
            // Gentle downhills allow recovery; steep downhills require braking.
            let g = p.grade;
            let factor = if g >= 0.0 {
                (-3.5 * g).exp()
            } else {
                (1.0 + (-g).min(0.06) * 2.0) * (-4.0 * (-g - 0.06).max(0.0)).exp()
            };
            (c.base_speed_mps * factor).max(0.05)
        })
        .collect();
    let at_scale = |k: f64| {
        constrain(
            natural
                .iter()
                .zip(&caps)
                .map(|(v, cap)| (v * k).min(*cap))
                .collect(),
        )
    };
    let Some(target) = c.target_duration_s else {
        return Ok((at_scale(1.0), fastest));
    };
    if target < fastest - 1e-6 {
        return Err(format!(
            "目标时长 {target:.3}s 不可行；当前地形、爬升及加速度约束下至少需要 {fastest:.3}s"
        ));
    }
    let mut lo = 0.0;
    let mut hi = natural
        .iter()
        .zip(&caps)
        .map(|(v, cap)| cap / v)
        .fold(1.0, f64::max);
    for _ in 0..80 {
        let mid = (lo + hi) / 2.0;
        if duration(&at_scale(mid)) > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok((at_scale(hi), fastest))
}

fn apply_motion(points: &mut [MotionPoint], speeds: &[f64]) {
    for i in 0..points.len() {
        let p = &mut points[i];
        p.speed_mps = speeds[i];
        let fatigue = route_planner::fatigue_from_km(p.distance_m / 1000.0);
        let target =
            route_planner::gait(speeds[i], fatigue).cadence * (1.0 + p.grade.clamp(0.0, 0.3) * 0.2);
        // Walking / slow climbing must also preserve v = cadence/60 * step_length.
        p.step_length_m = (speeds[i] * 60.0 / target).clamp(0.2, 1.7);
        p.cadence_spm = speeds[i] * 60.0 / p.step_length_m;
        if i > 0 {
            let dt = 2.0 * (points[i].distance_m - points[i - 1].distance_m)
                / (speeds[i] + speeds[i - 1]);
            points[i].time_s = points[i - 1].time_s + dt;
            points[i].cumulative_steps = points[i - 1].cumulative_steps
                + (points[i - 1].cadence_spm + points[i].cadence_spm) / 120.0 * dt;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terrain() -> TerrainGrid {
        serde_json::from_str(include_str!("../../examples/terrain/grid.json")).unwrap()
    }
    fn route() -> RouteInput {
        serde_json::from_str(include_str!("../../examples/terrain/route.json")).unwrap()
    }
    fn straight(uphill: bool) -> RouteInput {
        let mut points = vec![
            RouteCoordinate {
                latitude: 30.0002,
                longitude: 110.0002,
            },
            RouteCoordinate {
                latitude: 30.0018,
                longitude: 110.0002,
            },
        ];
        if !uphill {
            points.reverse();
        }
        RouteInput {
            coordinate_system: CoordinateSystem::Wgs84,
            closed: false,
            points,
        }
    }
    #[test]
    fn terrain_slows_climbing_and_preserves_corners_and_seam() {
        let c = SimulationConfig {
            max_climb_mps: 0.15,
            ..Default::default()
        };
        let up = simulate(&straight(true), &terrain(), &c).unwrap();
        let down = simulate(&straight(false), &terrain(), &c).unwrap();
        assert!(up.duration_s > down.duration_s * 1.5);
        assert!(up.validation.max_climb_mps <= 0.15 + 1e-8);
        let input = route();
        let looped = simulate(&input, &terrain(), &c).unwrap();
        for corner in input.points {
            assert!(looped
                .motion
                .iter()
                .any(|p| (p.latitude - corner.latitude).abs() < 1e-10
                    && (p.longitude - corner.longitude).abs() < 1e-10));
        }
        assert_eq!(
            looped.motion[0].ground_height_m,
            looped.motion.last().unwrap().ground_height_m
        );
        assert!((looped.ascent_m - looped.descent_m).abs() < 1e-8);
    }
    #[test]
    fn duration_fit_respects_caps_and_rejects_impossible_target() {
        let mut c = SimulationConfig::default();
        let natural = simulate(&route(), &terrain(), &c).unwrap();
        for target in [
            natural.duration_s * 1.5,
            (natural.duration_s + natural.fastest_duration_s) / 2.0,
            natural.fastest_duration_s,
        ] {
            c.target_duration_s = Some(target);
            let fitted = simulate(&route(), &terrain(), &c).unwrap();
            assert!((fitted.duration_s - target).abs() < 1e-5);
            assert!(fitted.validation.passed);
        }
        c.target_duration_s = Some(natural.fastest_duration_s * 0.5);
        assert!(simulate(&route(), &terrain(), &c)
            .unwrap_err()
            .contains("不可行"));
    }
    #[test]
    fn noise_does_not_change_physics_or_ascent_and_seed_replays() {
        let mut c = SimulationConfig {
            sensors: true,
            acceleration_hz: 10,
            ..Default::default()
        };
        let a = simulate(&straight(true), &terrain(), &c).unwrap();
        let replay = simulate(&straight(true), &terrain(), &c).unwrap();
        assert_eq!(
            serde_json::to_string(&a.sensors).unwrap(),
            serde_json::to_string(&replay.sensors).unwrap()
        );
        c.seed += 1;
        c.gnss_height_sigma_m = 8.0;
        let b = simulate(&straight(true), &terrain(), &c).unwrap();
        assert_eq!(a.ascent_m, b.ascent_m);
        assert_eq!(a.duration_s, b.duration_s);
        assert_ne!(
            serde_json::to_string(&a.sensors).unwrap(),
            serde_json::to_string(&b.sensors).unwrap()
        );
        let mut bad = b;
        bad.motion[2].time_s = bad.motion[1].time_s;
        assert!(bad
            .validate()
            .issues
            .iter()
            .any(|i| i.code == "motion_order"));
    }
    #[test]
    fn pressure_and_phase_damage_are_detected() {
        let c = SimulationConfig {
            sensors: true,
            acceleration_hz: 10,
            ..Default::default()
        };
        assert!((pressure_hpa(0.0, &c).unwrap() - 1013.25).abs() < 1e-8);
        assert!(pressure_hpa(100.0, &c).unwrap() < pressure_hpa(0.0, &c).unwrap());
        let mut out = simulate(&straight(true), &terrain(), &c).unwrap();
        let mut pressure_changed = false;
        let mut phase_changed = false;
        for sample in &mut out.sensors {
            match sample {
                SensorSample::Pressure { pressure_hpa, .. } if !pressure_changed => {
                    *pressure_hpa += 10.0;
                    pressure_changed = true;
                }
                SensorSample::Accelerometer { step_phase_rad, .. } if !phase_changed => {
                    *step_phase_rad += 1.0;
                    phase_changed = true;
                }
                _ => {}
            }
        }
        let report = out.validate();
        assert!(!report.passed);
        assert!(report.issues.iter().any(|i| i.code == "pressure_height"));
        assert!(report.issues.iter().any(|i| i.code == "acceleration_phase"));
        out.sensors.pop();
        assert!(out
            .validate()
            .issues
            .iter()
            .any(|i| i.code == "sensor_coverage"));
        out.duration_s = f64::NAN;
        assert!(out
            .validate()
            .issues
            .iter()
            .any(|i| i.code == "summary_value"));
    }
    #[test]
    fn missing_terrain_invalid_config_and_resource_limits_fail_cleanly() {
        let mut g = terrain();
        g.heights_m[0][0] = None;
        assert!(simulate(&route(), &g, &SimulationConfig::default())
            .unwrap_err()
            .contains("缺测"));
        for c in [
            SimulationConfig {
                spacing_m: f64::NAN,
                ..Default::default()
            },
            SimulationConfig {
                spacing_m: 1e-12,
                ..Default::default()
            },
            SimulationConfig {
                target_duration_s: Some(-1.0),
                ..Default::default()
            },
            SimulationConfig {
                sensors: true,
                target_duration_s: Some(1e6),
                ..Default::default()
            },
        ] {
            assert!(simulate(&route(), &terrain(), &c).is_err());
        }
    }
    #[test]
    fn spatial_filter_removes_isolated_spike_without_inflating_ascent() {
        let mut c = SimulationConfig {
            smoothing_radius_m: 0.0,
            ..Default::default()
        };
        let mut points = sample_route(&straight(true), &terrain(), 5.0).unwrap();
        let original = points[5].raw_height_m;
        points[5].raw_height_m += 40.0;
        filter_heights(&mut points, false, &c).unwrap();
        assert!(points[5].corrected_spike);
        assert!((points[5].ground_height_m - original).abs() < 1e-7);
        c.smoothing_radius_m = 15.0;
        filter_heights(&mut points, false, &c).unwrap();
        assert!(points
            .windows(2)
            .all(|p| p[1].ground_height_m >= p[0].ground_height_m));
    }
}
