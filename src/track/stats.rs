//! Derived statistics have one source: the final cumulative samples.
use super::geom::round_to;
use super::model::{GenPoint, Segment, TenWindow, Track};

/// Interpolate cumulative counters at a time boundary. Rounding cumulative steps
/// (rather than each interval) preserves the exact final step count.
pub fn counters_at(locs: &[GenPoint], time: i64) -> (f64, i64) {
    let Some(first) = locs.first() else {
        return (0.0, 0);
    };
    if time <= first.totalTime {
        return (first.totalDis, first.steps);
    }
    for pair in locs.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if b.totalTime >= time && b.totalTime > a.totalTime {
            let k = (time - a.totalTime) as f64 / (b.totalTime - a.totalTime) as f64;
            return (
                a.totalDis + (b.totalDis - a.totalDis) * k,
                (a.steps as f64 + (b.steps - a.steps) as f64 * k).round() as i64,
            );
        }
    }
    let last = locs.last().unwrap();
    (last.totalDis, last.steps)
}

pub fn rebuild(track: &mut Track) {
    normalize_step_fields(&mut track.locations);
    let Some(last) = track.locations.last() else {
        return;
    };
    track.totalTime = last.totalTime;
    track.totalDistance = last.totalDis;
    track.totalSteps = last.steps;
    track.validDistance = last.validDis;
    track.validTime = last.validTime;
    track.speedPerTenSec.clear();
    track.stepsPerTenSec.clear();
    track.segments.clear();
    for (width, segments) in [(10, false), (60, true)] {
        let mut lo = 0;
        let mut previous = track_counters_at(track, 0);
        while lo < track.totalTime {
            let hi = (lo + width).min(track.totalTime);
            let current = track_counters_at(track, hi);
            let distance = current.0 - previous.0;
            let steps = current.1 - previous.1;
            if segments {
                track.segments.push(Segment {
                    totalTime: hi - lo,
                    distance: current.0.round() as i64 - previous.0.round() as i64,
                    startTime: lo * 1000,
                    endTime: hi * 1000,
                    avgSpeed: distance / (hi - lo) as f64,
                    avgStep: (steps as f64 * 60.0 / (hi - lo) as f64).round() as i64,
                    state: 0,
                });
            } else {
                track.speedPerTenSec.push(TenWindow {
                    time: hi - lo,
                    value: distance,
                });
                track.stepsPerTenSec.push(TenWindow {
                    time: hi - lo,
                    value: steps as f64,
                });
            }
            previous = current;
            lo = hi;
        }
    }
}

/// 协议点 `count`/`stepDistance` 由累计值反推：`count = 本点步数增量`，
/// `stepDistance = 本段距离 / 本段步数`（夹在 0.5–1.8m），保证与 GPS 自洽。
fn normalize_step_fields(locs: &mut [GenPoint]) {
    let (mut prev_d, mut prev_steps) = (0.0f64, 0i64);
    for p in locs.iter_mut() {
        let steps = p.steps.max(prev_steps);
        let count = (steps - prev_steps).max(0);
        let distance = (p.totalDis - prev_d).max(0.0);
        p.count = count;
        p.stepDistance = if count > 0 {
            round_to((distance / count as f64).clamp(0.5, 1.8), 2)
        } else {
            0.0
        };
        prev_d = p.totalDis;
        prev_steps = steps;
    }
}

fn track_counters_at(track: &Track, time: i64) -> (f64, i64) {
    if let Some(simulation) = &track.simulation {
        if time >= track.totalTime {
            return (track.totalDistance, track.totalSteps);
        }
        let (distance, steps) = simulation.counters_at_time(time as f64);
        (distance, steps.round() as i64)
    } else {
        counters_at(&track.locations, time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_samples_drive_totals_and_partial_windows() {
        for duration in [1, 9, 10, 11, 59, 60, 61, 180, 603] {
            let mut t = crate::track::generator::build(
                1200.0,
                600,
                7,
                (39.9, 116.4),
                0,
                &[(39.9, 116.4), (39.901, 116.401)],
            );
            let mut a = t.locations[0].clone();
            a.totalTime = 0;
            a.totalDis = 0.0;
            a.steps = 0;
            let mut b = a.clone();
            b.totalTime = duration;
            b.totalDis = 1000.0;
            b.steps = 1201;
            b.validTime = duration - 1;
            b.validDis = 999.0;
            t.locations = vec![a, b];
            rebuild(&mut t);
            assert_eq!(t.totalTime, duration);
            assert_eq!(t.validTime, duration - 1);
            assert_eq!(t.validDistance, 999.0);
            assert_eq!(
                t.stepsPerTenSec.iter().map(|w| w.value as i64).sum::<i64>(),
                1201
            );
            assert!((t.speedPerTenSec.iter().map(|w| w.value).sum::<f64>() - 1000.0).abs() < 1e-8);
            assert_eq!(
                t.speedPerTenSec.iter().map(|w| w.time).sum::<i64>(),
                duration
            );
            assert_eq!(t.segments[0].startTime, 0);
            assert_eq!(t.segments.iter().map(|s| s.distance).sum::<i64>(), 1000);
            assert_eq!(t.segments.last().unwrap().endTime, duration * 1000);
            for pair in t.segments.windows(2) {
                assert_eq!(pair[0].endTime, pair[1].startTime);
            }
        }
    }
}
