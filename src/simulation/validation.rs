use super::{pressure_hpa, sensors::state_at, SensorSample, SimulationOutput};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub code: &'static str,
    pub index: usize,
    pub time_s: f64,
    pub message: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ValidationReport {
    pub passed: bool,
    pub checked_motion_intervals: usize,
    pub checked_sensor_samples: usize,
    pub max_climb_mps: f64,
    pub max_descent_mps: f64,
    pub max_acceleration_mps2: f64,
    pub max_braking_mps2: f64,
    pub max_pressure_residual_hpa: f64,
    pub max_gnss_height_residual_m: f64,
    pub issues: Vec<Issue>,
}

impl SimulationOutput {
    /// Can be rerun after editing/resampling the final output. Index refers to
    /// motion or sensor samples according to the error code.
    pub fn validate(&self) -> ValidationReport {
        let mut r = ValidationReport::default();
        let c = &self.config;
        let mut issue = |code, index, time_s, message: &str| {
            // Bound diagnostics for severely damaged input.
            if r.issues.len() < 100 {
                r.issues.push(Issue {
                    code,
                    index,
                    time_s,
                    message: message.into(),
                });
            }
        };
        if c.validate().is_err() {
            issue("configuration", 0, 0.0, "仿真配置无效");
            return r;
        }
        if ![
            self.distance_m,
            self.duration_s,
            self.ascent_m,
            self.descent_m,
            self.fastest_duration_s,
        ]
        .iter()
        .all(|x| x.is_finite() && *x >= 0.0)
        {
            issue("summary_value", 0, 0.0, "汇总包含非有限数或负数");
        }
        if self.motion.len() < 2 {
            issue("motion_count", 0, 0.0, "至少需要两个运动状态");
            return r;
        }
        let first = &self.motion[0];
        if first.time_s.abs() > 1e-8
            || first.distance_m.abs() > 1e-8
            || first.cumulative_steps.abs() > 1e-8
        {
            issue(
                "motion_origin",
                0,
                first.time_s,
                "起点时间、距离、步数必须为零",
            );
        }
        for (i, p) in self.motion.iter().enumerate() {
            if ![
                p.latitude,
                p.longitude,
                p.time_s,
                p.distance_m,
                p.ground_height_m,
                p.raw_height_m,
                p.grade,
                p.speed_mps,
                p.cadence_spm,
                p.step_length_m,
                p.cumulative_steps,
            ]
            .iter()
            .all(|v| v.is_finite())
                || p.speed_mps <= 0.0
                || p.cadence_spm <= 0.0
                || p.step_length_m <= 0.0
                || p.latitude.abs() > 85.0
                || p.longitude.abs() > 180.0
            {
                issue("motion_value", i, p.time_s, "运动状态包含无效数值");
            }
            if (p.speed_mps - p.cadence_spm / 60.0 * p.step_length_m).abs() > 1e-7 {
                issue("gait_coupling", i, p.time_s, "速度与步频、单步长度不一致");
            }
            if p.speed_mps > c.max_speed_mps + 1e-7 {
                issue("speed_limit", i, p.time_s, "超过全局速度限制");
            }
        }
        let (mut ascent, mut descent) = (0.0, 0.0);
        for (i, pair) in self.motion.windows(2).enumerate() {
            r.checked_motion_intervals += 1;
            let (a, b) = (&pair[0], &pair[1]);
            let dt = b.time_s - a.time_s;
            let ds = b.distance_m - a.distance_m;
            if dt <= 0.0 || ds <= 0.0 || !dt.is_finite() || !ds.is_finite() {
                issue("motion_order", i + 1, b.time_s, "时间或弧长未严格递增");
                continue;
            }
            if (ds - (a.speed_mps + b.speed_mps) * 0.5 * dt).abs() > 1e-6 * ds.max(1.0) {
                issue("distance_integral", i + 1, b.time_s, "位移与速度积分不一致");
            }
            let steps = (a.cadence_spm + b.cadence_spm) / 120.0 * dt;
            if (b.cumulative_steps - a.cumulative_steps - steps).abs() > 1e-6 {
                issue("step_integral", i + 1, b.time_s, "累计步数与步频积分不一致");
            }
            let grade = (b.ground_height_m - a.ground_height_m) / ds;
            ascent += (b.ground_height_m - a.ground_height_m).max(0.0);
            descent += (a.ground_height_m - b.ground_height_m).max(0.0);
            let vertical = grade * a.speed_mps.max(b.speed_mps);
            r.max_climb_mps = r.max_climb_mps.max(vertical.max(0.0));
            r.max_descent_mps = r.max_descent_mps.max((-vertical).max(0.0));
            if vertical > c.max_climb_mps + 1e-7 || -vertical > c.max_descent_mps + 1e-7 {
                issue("vertical_speed", i + 1, b.time_s, "区间垂直速度超出限制");
            }
            if (b.grade - grade).abs() > 1e-8 {
                issue("terrain_grade", i + 1, b.time_s, "坡度与地形剖面不一致");
            }
            let acceleration = (b.speed_mps - a.speed_mps) / dt;
            r.max_acceleration_mps2 = r.max_acceleration_mps2.max(acceleration.max(0.0));
            r.max_braking_mps2 = r.max_braking_mps2.max((-acceleration).max(0.0));
            if acceleration > c.max_acceleration_mps2 + 1e-7
                || -acceleration > c.max_braking_mps2 + 1e-7
            {
                issue("acceleration_limit", i + 1, b.time_s, "区间加减速超出限制");
            }
        }
        let last = self.motion.last().unwrap();
        if (last.distance_m - self.distance_m).abs() > 1e-7
            || (last.time_s - self.duration_s).abs() > 1e-7
            || (ascent - self.ascent_m).abs() > 1e-7
            || (descent - self.descent_m).abs() > 1e-7
            || c.target_duration_s
                .is_some_and(|t| (t - self.duration_s).abs() > 1e-5)
        {
            issue(
                "summary",
                self.motion.len() - 1,
                last.time_s,
                "汇总或目标时长与终点不一致",
            );
        }
        // Avoid interpolating malformed motion; report the structural errors first.
        if !r.issues.is_empty() {
            return r;
        }
        let mut previous = [-1.0; 3];
        let mut counts = [0usize; 3];
        let rates = [1.0, 5.0, c.acceleration_hz as f64];
        let mut merged_previous = -1.0;
        for (i, sample) in self.sensors.iter().enumerate() {
            r.checked_sensor_samples += 1;
            let time = sample.time_s();
            let channel = sample.channel();
            let message = if !time.is_finite()
                || time < 0.0
                || time > self.duration_s + 1e-8
                || time <= previous[channel]
                || time < merged_previous
                || (time - counts[channel] as f64 / rates[channel]).abs() > 1e-8
            {
                Some(("sensor_order", "传感器时间越界或顺序异常"))
            } else {
                let state = state_at(&self.motion, time);
                match sample {
                    SensorSample::GnssHeight {
                        observed_height_m, ..
                    } => {
                        let residual = (observed_height_m - state.height).abs();
                        r.max_gnss_height_residual_m = r.max_gnss_height_residual_m.max(residual);
                        if !observed_height_m.is_finite()
                            || residual > 6.0 * c.gnss_height_sigma_m + 2.0
                        {
                            Some(("gnss_height", "GNSS 高度无效或残差超过噪声模型容限"))
                        } else {
                            None
                        }
                    }
                    SensorSample::Pressure {
                        pressure_hpa: pressure,
                        ..
                    } => {
                        let expected = pressure_hpa(state.height, c).unwrap_or(f64::NAN);
                        let residual = (pressure - expected).abs();
                        r.max_pressure_residual_hpa = r.max_pressure_residual_hpa.max(residual);
                        if !pressure.is_finite()
                            || *pressure <= 0.0
                            || !expected.is_finite()
                            || residual > 0.5
                        {
                            Some((
                                "pressure_height",
                                "气压与地形高度残差超过 0.5 hPa 或数值无效",
                            ))
                        } else {
                            None
                        }
                    }
                    SensorSample::Accelerometer {
                        forward_left_up_mps2,
                        step_phase_rad,
                        ..
                    } => {
                        if !forward_left_up_mps2.iter().all(|v| v.is_finite())
                            || !step_phase_rad.is_finite()
                            || (step_phase_rad - state.phase).abs() > 1e-6
                        {
                            Some(("acceleration_phase", "加速度数值或步态相位不一致"))
                        } else {
                            None
                        }
                    }
                }
            };
            if let Some((code, message)) = message {
                if r.issues.len() < 100 {
                    r.issues.push(Issue {
                        code,
                        index: i,
                        time_s: time,
                        message: message.into(),
                    });
                }
            }
            previous[channel] = time;
            merged_previous = time;
            counts[channel] += 1;
        }
        for channel in 0..3 {
            let expected = if c.sensors {
                ((self.duration_s * rates[channel]).floor() as usize).saturating_add(1)
            } else {
                0
            };
            if counts[channel] != expected && r.issues.len() < 100 {
                r.issues.push(Issue {
                    code: "sensor_coverage",
                    index: channel,
                    time_s: self.duration_s,
                    message: format!("通道 {channel} 样本数 {}，期望 {expected}", counts[channel]),
                });
            }
        }
        r.passed = r.issues.is_empty();
        r
    }
}
