use super::*;
use crate::track::wire::{build_obs_object, conv_point};
use std::io::Read;

fn context() -> TerrainRunContext {
    TerrainRunContext {
        terrain: serde_json::from_str(include_str!("../../examples/terrain/grid.json")).unwrap(),
        config: SimulationConfig {
            acceleration_hz: 10,
            ..Default::default()
        },
    }
}
fn ring() -> Vec<(f64, f64)> {
    let route: RouteInput =
        serde_json::from_str(include_str!("../../examples/terrain/route.json")).unwrap();
    route
        .points
        .iter()
        .map(|p| {
            convert_coordinate(
                p.latitude,
                p.longitude,
                CoordinateSystem::Wgs84,
                CoordinateSystem::Bd09,
            )
        })
        .collect()
}
pub(crate) fn terrain_track() -> Track {
    generate(&ring(), 1400.0, 900, 42, 1_700_000_000_000, &context()).unwrap()
}
fn decode(obs: &serde_json::Value, key: &str) -> serde_json::Value {
    let bytes = crate::crypto::envelope::b64_decode(obs[key].as_str().unwrap()).unwrap();
    let mut plain = String::new();
    flate2::read::GzDecoder::new(&bytes[..])
        .read_to_string(&mut plain)
        .unwrap();
    serde_json::from_str(&plain).unwrap()
}

#[test]
fn adapter_maps_observations_units_time_steps_and_keeps_all_sensors() {
    let track = terrain_track();
    let simulation = track.simulation.as_ref().unwrap();
    let first_observed = match simulation.sensors[0] {
        SensorSample::GnssHeight {
            observed_height_m, ..
        } => observed_height_m,
        _ => panic!("expected first GNSS fix"),
    };
    assert_eq!(track.locations[0].bdA, round_to(first_observed, 2));
    assert!((track.locations[0].bdA - simulation.motion[0].ground_height_m).abs() > 0.01);
    assert!(simulation
        .sensors
        .iter()
        .any(|s| matches!(s, SensorSample::Pressure { .. })));
    assert!(simulation
        .sensors
        .iter()
        .any(|s| matches!(s, SensorSample::Accelerometer { .. })));
    assert_eq!(track.totalTime, 900);
    assert!((track.totalDistance - 1400.0).abs() < 0.001);
    assert_eq!(
        track.totalSteps,
        simulation.motion.last().unwrap().cumulative_steps.round() as i64
    );
    assert!(track
        .locations
        .windows(2)
        .all(|w| w[1].gainTimeMs > w[0].gainTimeMs));
    assert!(track
        .locations
        .iter()
        .any(|p| (p.gainTimeMs - track.startTime) % 1000 != 0));
    for (p, m) in track.locations.iter().zip(&simulation.motion) {
        assert_eq!(p.speed, round_to(m.speed_mps * 3.6, 4));
        assert_eq!(p.avgSpeed, round_to(m.speed_mps, 4));
        assert_eq!(
            p.gainTimeMs - track.startTime,
            (m.time_s * 1000.0).round() as i64
        );
        let wire = conv_point(p, track.startTime);
        assert_eq!(wire["bdA"], p.bdA);
        assert_eq!(wire["steps"], p.steps);
    }
    assert!(serde_json::to_value(&track)
        .unwrap()
        .get("simulation")
        .is_none());
    let mut timestamps = absolute_sensors(&track);
    assert_eq!(
        timestamps.next().unwrap().unwrap().timestamp_ms,
        track.startTime
    );
    assert_eq!(
        timestamps.next_back().unwrap().unwrap().timestamp_ms,
        track.startTime + 900_000
    );
}

#[test]
fn protocol_windows_laps_and_ascent_share_final_physics() {
    let track = terrain_track();
    let simulation = track.simulation.as_ref().unwrap();
    assert!(
        (track.speedPerTenSec.iter().map(|w| w.value).sum::<f64>() - track.totalDistance).abs()
            < 1e-6
    );
    assert_eq!(
        track
            .stepsPerTenSec
            .iter()
            .map(|w| w.value as i64)
            .sum::<i64>(),
        track.totalSteps
    );
    assert_eq!(track.total_ascent(), simulation.ascent_m);
    let obs = build_obs_object(&track, 42, "fixture", 7, "{}");
    // Known 10-key schema stays unchanged; raw sensors remain attached to Track.
    assert_eq!(obs.as_object().unwrap().len(), 10);
    let wrapper = decode(&obs, "run_data");
    let points: Vec<serde_json::Value> =
        serde_json::from_str(wrapper["allLocJson"].as_str().unwrap()).unwrap();
    assert_eq!(points.len(), track.locations.len());
    assert_eq!(points.last().unwrap()["totalTime"], 900);
    assert_eq!(points.last().unwrap()["steps"], track.totalSteps);
    let laps = decode(&obs, "laps_json");
    let laps = laps.as_array().unwrap();
    assert_eq!(laps.len(), 2);
    assert!(
        (laps
            .iter()
            .map(|l| l["elevationGain"].as_f64().unwrap())
            .sum::<f64>()
            - simulation.ascent_m)
            .abs()
            < 0.02
    );
    assert_eq!(
        laps.iter()
            .map(|l| l["step"].as_i64().unwrap())
            .sum::<i64>(),
        track.totalSteps
    );
    assert_eq!(
        laps.iter()
            .map(|l| l["duration"].as_i64().unwrap())
            .sum::<i64>(),
        900
    );
}

#[test]
fn unsafe_postprocessing_and_impossible_targets_do_not_fall_back() {
    assert!(generate(&ring(), 1400.0, 10, 42, 0, &context())
        .unwrap_err()
        .contains("不可行"));
    let mut track = terrain_track();
    assert!(super::super::altitude::override_bd_a(&mut track, 100.0).is_err());
    track.locations[4].speed += 1.0;
    assert!(track.validate_consistency().is_err());
    let mut track = terrain_track();
    track.locations[4].gLat += 0.001;
    assert!(track.validate_consistency().is_err());
}

#[test]
fn old_config_is_disabled_and_enabled_settings_validate_files_and_conflicts() {
    let old: crate::api::model::Config = serde_json::from_str("{}").unwrap();
    assert!(!old.terrain_simulation.enabled);
    assert!(old.terrain_simulation.load().unwrap().is_none());
    let enabled = TerrainSimulationSettings {
        enabled: true,
        ..Default::default()
    };
    assert!(enabled.load().is_err());
    assert!(enabled.validate_altitude(Some(1.0), None).is_err());
    let enabled = TerrainSimulationSettings {
        enabled: true,
        terrain_path: concat!(env!("CARGO_MANIFEST_DIR"), "/examples/terrain/grid.json").into(),
        ..Default::default()
    };
    assert!(enabled.load().unwrap().is_some());
}

#[test]
fn both_generators_use_new_physics_and_keep_checkpoints() {
    use route_planner::{Coord, RoadGraph};
    let ring = ring();
    // Leave coverage for Catmull-Rom's existing corner overshoot as well as the
    // small inverse map-coordinate conversion error near raster boundaries.
    let center = (
        ring[..4].iter().map(|p| p.0).sum::<f64>() / 4.0,
        ring[..4].iter().map(|p| p.1).sum::<f64>() / 4.0,
    );
    let inner: Vec<_> = ring[..4]
        .iter()
        .map(|p| {
            (
                center.0 + (p.0 - center.0) * 0.75,
                center.1 + (p.1 - center.1) * 0.75,
            )
        })
        .collect();
    let points = &inner[..];
    let legacy =
        super::super::generator::build_simulated(1400.0, 900, 42, 0, points, &context()).unwrap();
    assert!(legacy.simulation.is_some());
    let mut net = RoadGraph::new();
    let nodes: Vec<_> = points
        .iter()
        .map(|p| net.add_node(Coord::new(p.1, p.0)))
        .collect();
    for i in 0..4 {
        let j = (i + 1) % 4;
        let edge = route_planner::graph::EdgeData {
            way_id: i as u64,
            oneway: false,
            highway: "footway".into(),
            maxspeed_kmh: None,
            length_m: haversine_latlng(points[i], points[j]),
        };
        net.add_edge(nodes[i], nodes[j], edge.clone());
        net.add_edge(nodes[j], nodes[i], edge);
    }
    net.rebuild_index();
    let road = super::super::generate_road::build_road_simulated(
        1400.0,
        900,
        42,
        0,
        points,
        points,
        &net,
        &context(),
    )
    .unwrap();
    assert!(road.simulation.is_some());
    for track in [legacy, road] {
        track.validate_consistency().unwrap();
        assert_eq!(track.totalTime, 900);
        assert!(points
            .iter()
            .all(|&p| super::super::wire::passes_point(&track, p)));
        assert!(track
            .locations
            .iter()
            .all(|p| p.ptype != -1 && p.ptype != 5));
    }
}
