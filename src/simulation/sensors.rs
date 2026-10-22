use super::{MotionPoint, SimulationConfig};
use rand::{rngs::StdRng, SeedableRng};
use rand_distr::{Distribution, StandardNormal};
use serde::Serialize;

const GRAVITY: f64 = 9.80665;
const LAPSE_RATE: f64 = 0.0065;
const GAS_CONSTANT: f64 = 287.05;
const MAX_SENSOR_SAMPLES: usize = 2_000_000;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "sensor", rename_all = "snake_case")]
pub enum SensorSample {
    GnssHeight {
        time_s: f64,
        observed_height_m: f64,
    },
    Pressure {
        time_s: f64,
        pressure_hpa: f64,
    },
    /// Ideal aligned frame: x forward, y left, z up. Includes +g at rest.
    /// This is not a device-attitude or clinical biomechanics model.
    Accelerometer {
        time_s: f64,
        forward_left_up_mps2: [f64; 3],
        step_phase_rad: f64,
    },
}

impl SensorSample {
    pub fn time_s(&self) -> f64 {
        match self {
            Self::GnssHeight { time_s, .. }
            | Self::Pressure { time_s, .. }
            | Self::Accelerometer { time_s, .. } => *time_s,
        }
    }
    pub(super) fn channel(&self) -> usize {
        match self {
            Self::GnssHeight { .. } => 0,
            Self::Pressure { .. } => 1,
            Self::Accelerometer { .. } => 2,
        }
    }
}

/// Tropospheric ISA relation anchored to a declared reference height/pressure.
/// Heights must use one datum. Temperature is at reference_height_m.
pub fn pressure_hpa(height_m: f64, config: &SimulationConfig) -> Result<f64, String> {
    let ratio =
        1.0 - LAPSE_RATE * (height_m - config.reference_height_m) / config.reference_temperature_k;
    if !ratio.is_finite()
        || ratio <= 0.0
        || !config.reference_pressure_hpa.is_finite()
        || config.reference_pressure_hpa <= 0.0
        || config.reference_temperature_k <= 0.0
        || !config.reference_temperature_k.is_finite()
    {
        return Err("气压参考参数超出对流层模型有效范围".into());
    }
    let pressure =
        config.reference_pressure_hpa * ratio.powf(GRAVITY / (GAS_CONSTANT * LAPSE_RATE));
    if !pressure.is_finite() || pressure <= 0.0 {
        return Err("气压模型产生无效值".into());
    }
    Ok(pressure)
}

pub(super) struct State {
    pub height: f64,
    pub acceleration: f64,
    pub phase: f64,
}

/// Motion is constant-acceleration in each spatial interval. Step rate is linear
/// in time, so phase is the integral of cadence rather than f(t)*t.
pub(super) fn state_at(points: &[MotionPoint], time: f64) -> State {
    let i = points
        .partition_point(|p| p.time_s < time)
        .clamp(1, points.len() - 1);
    let (a, b) = (&points[i - 1], &points[i]);
    let dt = b.time_s - a.time_s;
    let elapsed = (time - a.time_s).clamp(0.0, dt);
    let acceleration = (b.speed_mps - a.speed_mps) / dt;
    let ds = a.speed_mps * elapsed + 0.5 * acceleration * elapsed * elapsed;
    let fraction = (ds / (b.distance_m - a.distance_m)).clamp(0.0, 1.0);
    let steps = a.cumulative_steps
        + a.cadence_spm / 60.0 * elapsed
        + (b.cadence_spm - a.cadence_spm) / 120.0 / dt * elapsed * elapsed;
    State {
        height: a.ground_height_m + (b.ground_height_m - a.ground_height_m) * fraction,
        acceleration,
        phase: steps * std::f64::consts::TAU,
    }
}

fn noise(rng: &mut StdRng) -> f64 {
    StandardNormal.sample(rng)
}

pub(super) fn generate(
    points: &[MotionPoint],
    c: &SimulationConfig,
) -> Result<Vec<SensorSample>, String> {
    let duration = points.last().unwrap().time_s;
    let count = duration * (c.acceleration_hz as f64 + 6.0) + 3.0;
    if !count.is_finite() || count > MAX_SENSOR_SAMPLES as f64 {
        return Err("传感器样本预计超过 2000000，请降低采样率、缩短路线或关闭 sensors".into());
    }
    let mut result = Vec::with_capacity(count as usize);
    // Independent streams: enabling another sensor cannot change existing observations.
    let mut gnss_rng = StdRng::seed_from_u64(c.seed ^ 0x4750_5301);
    let mut pressure_rng = StdRng::seed_from_u64(c.seed ^ 0x5052_5302);
    let mut accel_rng = StdRng::seed_from_u64(c.seed ^ 0x4143_4303);
    let mut drift = 0.0;
    for i in 0..=(duration.floor() as usize) {
        let time_s = i as f64;
        drift = 0.98 * drift + 0.03 * noise(&mut gnss_rng);
        let observed_height_m =
            state_at(points, time_s).height + drift + c.gnss_height_sigma_m * noise(&mut gnss_rng);
        result.push(SensorSample::GnssHeight {
            time_s,
            observed_height_m,
        });
    }
    let mut pressure = pressure_hpa(points[0].ground_height_m, c)?;
    let mut drift = 0.0;
    for i in 0..=((duration * 5.0).floor() as usize) {
        let time_s = i as f64 / 5.0;
        let expected = pressure_hpa(state_at(points, time_s).height, c)?;
        pressure += (1.0 - (-0.2_f64 / 0.5).exp()) * (expected - pressure);
        drift = 0.995 * drift + 0.001 * noise(&mut pressure_rng);
        result.push(SensorSample::Pressure {
            time_s,
            pressure_hpa: pressure + drift + 0.015 * noise(&mut pressure_rng),
        });
    }
    for i in 0..=((duration * c.acceleration_hz as f64).floor() as usize) {
        let time_s = i as f64 / c.acceleration_hz as f64;
        let state = state_at(points, time_s);
        let phase = state.phase;
        // Illustrative support/swing harmonics, not a calibrated person-specific waveform.
        let forward_left_up_mps2 = [
            state.acceleration + 1.2 * phase.sin() + 0.05 * noise(&mut accel_rng),
            0.4 * (phase * 0.5).sin() + 0.05 * noise(&mut accel_rng),
            GRAVITY + 2.0 * phase.cos() + 0.6 * (2.0 * phase).cos() + 0.05 * noise(&mut accel_rng),
        ];
        result.push(SensorSample::Accelerometer {
            time_s,
            forward_left_up_mps2,
            step_phase_rad: phase,
        });
    }
    result.sort_by(|a, b| a.time_s().total_cmp(&b.time_s()));
    Ok(result)
}
