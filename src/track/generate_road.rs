//! 真实道路路由轨迹生成器（模式 B）。
//!
//! 与经典模式（`generator::build`）的分工：
//! - 几何源为路网闭环路线（而非 Catmull-Rom 环）；
//! - 配速为运动学速度剖面：OU 均值回归 + 弯道预判刹车（`kinematics::pace_profile`）；
//! - 步频/步幅为生物力学 OU 耦合（`biomech::ou_cadence_series` + `gait`）；
//! - GPS 抖动为 AR(1) 相关漂移 + 高斯测量噪声 + 建筑 SDF 衰减（`noise::GpsJitter`）。
//!
//! 协议字段/哨兵/断崖/吸附/10s 窗与经典模式完全一致（共用 `synth`）。

use std::collections::HashMap;

use super::generator::{SPEED_CEIL, SPEED_FLOOR};
pub use super::geom::radial_order;
use super::geom::{dist2_m, ring_point_at, round_to, wgs84_to_bd09, LocalFrame, Rng};
use super::model::{GenPoint, Track};
use super::postfix::apply_post_fixes;
use super::synth::{self, Altitude, Sample};
#[cfg(any(feature = "gui", test))]
use route_planner::check::Check;
use route_planner::check::Inspection;
use route_planner::{
    fatigue_from_km, gait, ou_cadence_series, pace_profile, Coord, GpsJitter, KinParams, RoadGraph,
    Route, RouteOptions, Sdf,
};

/// 路线算法模式（二选一）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RouteMode {
    /// 经典打卡点环（Catmull-Rom）。
    #[default]
    Legacy,
    /// 真实道路拓扑路由。
    Road,
}

impl RouteMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "road" => RouteMode::Road,
            _ => RouteMode::Legacy,
        }
    }
}

/// 从本地文件加载 OSM 路网。
pub fn load_network_path(path: &str) -> Result<RoadGraph, String> {
    if path.trim().is_empty() {
        return Err("未配置 OSM 路网，请先导入路网或主动切换经典模式".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("读取 {path} 失败: {e}"))?;
    let xml = std::str::from_utf8(&bytes)
        .map_err(|_| "文件不是 OSM XML；PBF 等二进制格式请先转换为 .osm XML")?;
    if !xml.contains("<osm") || !xml.contains("</osm>") || xml.contains("<remark") {
        return Err("文件不是完整的 OSM XML 或包含地图服务错误；请重新导出 .osm/.xml".into());
    }
    let net = route_planner::load_osm(&bytes)?;
    if net.graph.edge_count() == 0
        || net.graph.node_weights().any(|n| {
            !n.coord.lat.is_finite()
                || !n.coord.lon.is_finite()
                || n.coord.lat.abs() > 90.
                || n.coord.lon.abs() > 180.
        })
    {
        return Err("路网没有可用道路或包含无效坐标，请检查 OSM 文件".into());
    }
    Ok(net)
}

#[cfg(any(feature = "gui", test))]
/// 路网预览计划（供地图显示）：对齐后的路网/建筑 + 生成的路线（BD 系）。
// 多数字段只由 GUI 地图读取。
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub struct RoadPlan {
    pub arcs: Vec<f64>,
    pub laps: Vec<f64>,
    pub route: Vec<(f64, f64)>,
    pub length_m: f64,
    pub edges: Vec<Vec<(f64, f64)>>,
    pub buildings: Vec<Vec<(f64, f64)>>,
    pub checkpoints: Vec<(f64, f64)>,
    pub fences: Vec<Vec<(f64, f64)>>,
}

/// 用围栏（BD 系，与打卡点同系）裁剪路网：仅保留围栏内道路；空路网交由路由器明确报错。
pub fn apply_fences(net: &RoadGraph, fences: &[Vec<(f64, f64)>]) -> RoadGraph {
    let mut g = net.clone();
    if !fences.is_empty() {
        clip_to_fences(&mut g, fences);
        g.rebuild_index();
    }
    g
}

/// 原地裁剪（不重建空间索引，由调用方重建）。
fn clip_to_fences(g: &mut RoadGraph, fences: &[Vec<(f64, f64)>]) {
    if fences.is_empty() {
        return;
    }
    let polys: Vec<Vec<Coord>> = fences
        .iter()
        .map(|r| r.iter().map(|p| Coord::new(p.1, p.0)).collect())
        .collect();
    g.graph
        .retain_nodes(|graph, n| route_planner::boundary::contains(&polys, graph[n].coord));
    // 进一步裁剪：移除「穿越围栏边界」的边（端点在内但中段跑出围栏，如跨校区道路）。
    // 双向道路的两条有向边共用一次判定。
    let mut inside: HashMap<(usize, usize), bool> = HashMap::new();
    g.graph.retain_edges(|graph, e| {
        let (a, b) = graph.edge_endpoints(e).unwrap();
        let key = (a.index().min(b.index()), a.index().max(b.index()));
        *inside.entry(key).or_insert_with(|| {
            route_planner::boundary::segment_inside(&polys, graph[a].coord, graph[b].coord)
        })
    });
    g.boundaries = polys;
}

#[cfg(any(feature = "gui", test))]
/// 路网准备：WGS84→BD-09 对齐 + 围栏裁剪（只克隆一次），预览与生成共用。
pub fn prepare_network(net: &RoadGraph, fences: &[Vec<(f64, f64)>]) -> RoadGraph {
    let mut g = net.clone();
    align_coords(&mut g);
    clip_to_fences(&mut g, fences);
    g.rebuild_index();
    g
}

/// 路线检查（供预览显示与生成复用）：保留检查阶段的投影路网与最短路。
pub fn inspect_points(g: &RoadGraph, points_bd: &[(f64, f64)], dist: f64) -> Inspection {
    let waypoints: Vec<Coord> = points_bd.iter().map(|p| Coord::new(p.1, p.0)).collect();
    route_planner::check::inspect_full(g, &waypoints, dist)
}

#[cfg(any(feature = "gui", test))]
/// 生成路网预览：在 [`prepare_network`] 准备好的路网上检查一次，并复用检查结果规划路线，
/// 不产生协议字段。预览与生成使用同一个路由函数。
///
/// 返回的检查结果总会给出（围栏校验失败时也基于裁剪后的路网检查）；
/// `cancelled` 在每个耗时阶段之间调用，返回错误即中止（此时检查为空）。
pub fn preview_prepared(
    g: &RoadGraph,
    points_bd: &[(f64, f64)],
    dist: f64,
    dur: i64,
    fences: &[Vec<(f64, f64)>],
    cancelled: impl Fn() -> Result<(), String>,
) -> (Check, Result<RoadPlan, String>) {
    if let Err(e) = cancelled() {
        return (Check::default(), Err(e));
    }
    let inspection = inspect_points(g, points_bd, dist);
    let plan = cancelled()
        .and_then(|_| {
            if fences.is_empty() {
                Ok(())
            } else {
                crate::api::school_roads::validate_fences(fences)
            }
        })
        .and_then(|_| route_from_inspection(g, &inspection, points_bd, dist, dur))
        .and_then(|route| {
            cancelled()?;
            Ok(road_plan(g, &route, points_bd, dist, fences))
        });
    (inspection.check, plan)
}

#[cfg(any(feature = "gui", test))]
fn road_plan(
    g: &RoadGraph,
    route: &Route,
    points_bd: &[(f64, f64)],
    dist: f64,
    fences: &[Vec<(f64, f64)>],
) -> RoadPlan {
    let edges = g
        .edges_deg()
        .iter()
        .map(|e| e.iter().map(|c| (c.lat, c.lon)).collect())
        .collect();
    let buildings = g
        .buildings
        .iter()
        .map(|r| r.iter().map(|c| (c.lat, c.lon)).collect())
        .collect();
    RoadPlan {
        arcs: route.points.iter().map(|p| p.s).collect(),
        laps: super::route_view::laps(route, dist),
        route: route.points.iter().map(|p| (p.lat, p.lon)).collect(),
        length_m: route.length_m,
        edges,
        buildings,
        checkpoints: points_bd.to_vec(),
        fences: fences.to_vec(),
    }
}

/// 将 WGS84 路网逐节点转换为 BD-09（与打卡点同系）。
///
/// OSM 为 WGS84，打卡点为 BD-09。两者不是常数平移关系（境内 GCJ/BD 偏移随位置
/// 变化），故用标准 WGS84→GCJ-02→BD-09 变换逐节点转换，校园尺度下与真实道路
/// 对齐（优于旧版质心常数平移的米级~数十米残差）。
pub fn align_network(net: &mut RoadGraph) {
    if net.graph.node_count() == 0 {
        return;
    }
    align_coords(net);
    net.rebuild_index();
}

/// 对齐节点与建筑坐标并重算投影锚点（不重建索引）。
fn align_coords(net: &mut RoadGraph) {
    if net.graph.node_count() == 0 {
        return;
    }
    for node in net.graph.node_weights_mut() {
        let (lat, lon) = wgs84_to_bd09(node.coord.lat, node.coord.lon);
        node.coord.lat = lat;
        node.coord.lon = lon;
    }
    for ring in net.buildings.iter_mut() {
        for c in ring.iter_mut() {
            let (lat, lon) = wgs84_to_bd09(c.lat, c.lon);
            c.lat = lat;
            c.lon = lon;
        }
    }
    net.compute_anchor();
}

/// 路线 → 平面折线 + 弧长表。
///
/// `arcs[k]` 必须与 `dense[k]` 一一对应（`ring_point_at` 按同下标取弧长），
/// 因此直接取每个采样点的累计弧长 `p.s`（`dense` 末点已闭合回起点）。
fn route_ring(route: &Route, frame: &LocalFrame) -> (Vec<(f64, f64)>, Vec<f64>) {
    let dense = route
        .points
        .iter()
        .map(|p| frame.to_xy(p.lat, p.lon))
        .collect();
    let arcs = route.points.iter().map(|p| p.s).collect();
    (dense, arcs)
}

pub fn effective_duration(dist: f64, dur: i64) -> i64 {
    let speed = (dist / dur.max(1) as f64).clamp(SPEED_FLOOR + 0.1, SPEED_CEIL - 0.1);
    (dist / speed).round().max(1.0) as i64
}

/// Share road routing with the old generator, but replace its entire sampling,
/// motion and post-processing pipeline when terrain simulation is enabled.
#[allow(clippy::too_many_arguments)]
pub fn build_road_simulated(
    dist: f64,
    dur: i64,
    seed: u64,
    start_ms: i64,
    route_bd: &[(f64, f64)],
    must_bd: &[(f64, f64)],
    net: &RoadGraph,
    context: &super::simulation_adapter::TerrainRunContext,
) -> Result<Track, String> {
    let mut required = route_bd.to_vec();
    for point in must_bd {
        if !required.contains(point) {
            required.push(*point);
        }
    }
    let route = required_route(net, &required, dist, dur)?;
    let ring: Vec<_> = route.points.iter().map(|p| (p.lat, p.lon)).collect();
    let track = super::simulation_adapter::generate(&ring, dist, dur, seed, start_ms, context)?;
    for &point in &required {
        if !super::wire::passes_point(&track, point) {
            return Err("最终山地道路轨迹未经过全部点位，请增加距离或修复路网".into());
        }
    }
    Ok(track)
}

fn route_options(dist: f64, dur: i64) -> Result<RouteOptions, String> {
    if !dist.is_finite() || dist <= 0.0 || dur <= 0 {
        return Err("距离和时长必须为正数".into());
    }
    let avg = dist / effective_duration(dist, dur) as f64;
    Ok(RouteOptions {
        min_radius_m: (avg * avg / 2.5).max(6.0),
        ..Default::default()
    })
}

fn required_route(
    net: &RoadGraph,
    points: &[(f64, f64)],
    dist: f64,
    dur: i64,
) -> Result<Route, String> {
    route_options(dist, dur)?;
    let inspection = inspect_points(net, points, dist);
    route_from_inspection(net, &inspection, points, dist, dur)
}

/// 由检查结果得到经过全部点位的闭环（检查阶段的投影与最短路直接复用）。
fn route_from_inspection(
    net: &RoadGraph,
    inspection: &Inspection,
    points: &[(f64, f64)],
    dist: f64,
    dur: i64,
) -> Result<Route, String> {
    let opts = route_options(dist, dur)?;
    if !net.boundaries.is_empty()
        && points
            .iter()
            .any(|p| !route_planner::boundary::contains(&net.boundaries, Coord::new(p.1, p.0)))
    {
        return Err("点位在学校围栏外，无法生成校内路线".into());
    }
    if let Some(error) = inspection.check.error() {
        return Err(error);
    }
    let route = inspection.route(&opts)?;
    if route.points.len() < 2 || !route.length_m.is_finite() || route.length_m <= 0.0 {
        return Err("路网无法形成有效闭环".into());
    }
    for (i, &point) in points.iter().enumerate() {
        let nearest = route
            .points
            .iter()
            .map(|p| dist2_m((p.lat, p.lon), point))
            .fold(f64::INFINITY, f64::min)
            .sqrt();
        if nearest > 40.0 {
            return Err(format!(
                "路网无法到达第 {} 个点位（最近 {:.1} 米，超过 40 米）",
                i + 1,
                nearest
            ));
        }
    }
    Ok(route)
}

/// 模式 B：真实道路拓扑路由轨迹生成。
///
/// `net` 须已对齐/裁剪（见 [`prepare_network`]）。`route_bd[0]` 为起点；
/// `route_bd` 和 `must_bd` 合并为强制必经点。
/// 不使用离路吸附；平滑路线和最终采样均须经过全部点位的 40 米范围。
pub fn build_road(
    dist: f64,
    dur: i64,
    seed: u64,
    start_ms: i64,
    route_bd: &[(f64, f64)],
    must_bd: &[(f64, f64)],
    net: &RoadGraph,
) -> Result<Track, String> {
    if dur <= 0 || !dist.is_finite() || dist <= 0.0 {
        return Err("距离和时长必须为正数".into());
    }
    let mut rng = Rng::new(seed);
    let dur_f = dur as f64;
    let avg_v = dist / dur_f;
    let a_c_max = 2.5;
    let mut required = route_bd.to_vec();
    for p in must_bd {
        if !required.contains(p) {
            required.push(*p);
        }
    }
    let route = required_route(net, &required, dist, dur)?;

    let frame = LocalFrame::centered(route_bd);
    let (dense, arcs) = route_ring(&route, &frame);

    // 时间网格（同经典模式）
    let (times, dts) = synth::time_grid(&mut rng, dur_f);
    let n = times.len();

    // 运动学速度剖面：OU 配速 + 弯道预判刹车 + 精确命中目标距离
    let kin = KinParams {
        a_c_max,
        ..Default::default()
    };
    let speed_seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let speeds = pace_profile(
        &route,
        avg_v,
        &dts,
        dist,
        speed_seed,
        &kin,
        SPEED_FLOOR,
        SPEED_CEIL,
    );
    let seg_dist: Vec<f64> = (0..n).map(|i| speeds[i] * dts[i]).collect();

    // 点位类型（同经典模式）
    let kinds = synth::assign_kinds(&mut rng, n);
    let disp_of = synth::distribute(&kinds, &seg_dist, dist);

    // 建筑 SDF（自适应放大漂移）
    let sdf = Sdf::from_buildings(
        &net.buildings,
        |c| {
            let (x, y) = frame.to_xy(c.lat, c.lon);
            [x, y]
        },
        2.0,
        30.0,
    );

    // 生物力学：步频在目标值附近 OU 波动，步幅反解严格满足 v=(cad/60)·stride
    let targets: Vec<f64> = (0..n)
        .map(|i| {
            let km_done = (times[i] / dur_f) * dist / 1000.0;
            gait(speeds[i], fatigue_from_km(km_done)).cadence
        })
        .collect();
    let cadence_seed = seed.wrapping_add(0xC6A4_A793_5BD1_E995);
    let cadences = ou_cadence_series(&targets, &dts, cadence_seed, 0.25, 0.5, 110.0, 220.0);

    // GPS 抖动：AR(1) 相关漂移 + 高斯测量噪声（SDF 逐点放大）
    let mut jitter = GpsJitter::new(0.72, 0.75, 0.3);
    let on_road = !net.boundaries.is_empty();

    let mut locs: Vec<GenPoint> = Vec::with_capacity(n);
    let mut positions = Vec::with_capacity(n);
    let mut s = 0.0f64;
    let mut t_acc = 0.0f64;
    let mut dist_acc = 0.0f64;
    let mut steps_acc = 0.0f64;
    let mut altitude = Altitude::new(&mut rng, dur_f);
    let pos = |ss: f64| ring_point_at(&dense, &arcs, ss);
    for i in 0..n {
        let dt = dts[i];
        let typ = kinds[i].0;
        t_acc += dt;
        let mut d_step = 0.0f64;
        if typ != -1 {
            d_step = disp_of[i];
            s += d_step;
        }
        let (x, y) = pos(s);
        let (px, py, (rad, state)) = if typ != -1 {
            // AR(1) 相关漂移 + 高斯测量噪声，SDF 自适应放大
            let sc = sdf.sigma_scale([x, y]);
            let z = [
                rng.gauss(0.0, 1.0),
                rng.gauss(0.0, 1.0),
                rng.gauss(0.0, 1.0),
                rng.gauss(0.0, 1.0),
            ];
            let (jxd, jyd) = jitter.step(sc, z);
            (x + jxd, y + jyd, synth::normal_fix(&mut rng, typ))
        } else {
            let (drift, ang) = synth::exception_drift(&mut rng);
            d_step = drift;
            let (px, py) = match (ang, locs.last()) {
                (Some(ang), _) => (x + drift * ang.sin(), y + drift * ang.cos()),
                (None, Some(last)) => frame.to_xy(last.gLat, last.gLng),
                (None, None) => {
                    let (jx, jy) = jitter.state();
                    (x + jx, y + jy)
                }
            };
            (px, py, synth::exception_fix(&mut rng))
        };
        if typ != -1 {
            dist_acc += d_step;
        }
        positions.push(s);
        // Boundary mode follows the actual road, including exceptional samples.
        let (lat, lng) = if on_road {
            frame.to_bd(x, y)
        } else {
            frame.to_bd(px, py)
        };
        let alt = altitude.step(&mut rng);
        // 生物力学联动：v=(cad/60)·stride
        let v_now = speeds[i];
        let stride = (v_now * 60.0 / cadences[i]).clamp(0.5, 1.7);
        let cad = v_now * 60.0 / stride;
        steps_acc += cad / 60.0 * dt;
        let nxt = pos(s + 2.0);
        let sample = Sample {
            index: i,
            kind: kinds[i],
            g_lat: if on_road { lat } else { round_to(lat, 7) },
            g_lng: if on_road { lng } else { round_to(lng, 7) },
            heading_deg: (nxt.0 - x).atan2(nxt.1 - y).to_degrees(),
            d_step,
            dt,
            t_acc,
            dist_acc,
            steps_acc,
            alt,
            radius: rad,
            state,
        };
        locs.push(synth::emit_point(&mut rng, &sample, start_ms));
    }
    apply_post_fixes(&mut locs, &mut rng, start_ms);
    if on_road {
        locs = super::school_track::preserve_corners(
            locs,
            &positions,
            &route,
            &net.boundaries,
            start_ms,
        )?;
    }

    let track = synth::assemble(locs, t_acc, dist, steps_acc, start_ms);
    track.validate_consistency()?;
    for point in route_bd.iter().chain(must_bd.iter()) {
        if !super::wire::passes_point(&track, *point) {
            return Err("最终道路轨迹未经过全部点位，请调整距离或路网".into());
        }
    }
    Ok(track)
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    fn plan_road_view(
        net: &RoadGraph,
        points_bd: &[(f64, f64)],
        dist: f64,
        dur: i64,
        fences: &[Vec<(f64, f64)>],
    ) -> Result<RoadPlan, String> {
        let g = prepare_network(net, fences);
        preview_prepared(&g, points_bd, dist, dur, fences, || Ok(())).1
    }
    #[test]
    fn preview_and_generation_share_required_points_and_options() {
        let mut net = RoadGraph::new();
        let wgs = [
            (39.9, 116.4),
            (39.9, 116.402),
            (39.902, 116.402),
            (39.902, 116.4),
        ];
        let nodes: Vec<_> = wgs
            .iter()
            .map(|&(lat, lon)| net.add_node(Coord::new(lon, lat)))
            .collect();
        for i in 0..4 {
            let edge = route_planner::graph::EdgeData {
                way_id: i as u64,
                oneway: false,
                highway: "footway".into(),
                maxspeed_kmh: None,
                length_m: 200.0,
            };
            net.add_edge(nodes[i], nodes[(i + 1) % 4], edge.clone());
            net.add_edge(nodes[(i + 1) % 4], nodes[i], edge);
        }
        net.compute_anchor();
        net.rebuild_index();
        let points: Vec<_> = wgs.iter().map(|&(a, b)| wgs84_to_bd09(a, b)).collect();
        let fences = vec![vec![
            (39.89, 116.39),
            (39.89, 116.42),
            (39.92, 116.42),
            (39.92, 116.39),
        ]
        .into_iter()
        .map(|(a, b)| wgs84_to_bd09(a, b))
        .collect()];
        let preview = plan_road_view(&net, &points, 2000.0, 800, &fences).unwrap();
        let mut aligned = net.clone();
        align_network(&mut aligned);
        let aligned = apply_fences(&aligned, &fences);
        let route = required_route(&aligned, &points, 2000.0, 800).unwrap();
        assert_eq!(preview.checkpoints, points);
        assert_eq!(
            preview.route,
            route
                .points
                .iter()
                .map(|p| (p.lat, p.lon))
                .collect::<Vec<_>>()
        );
        let track = build_road(2000.0, 800, 7, 0, &points, &points, &aligned).unwrap();
        assert!(points
            .iter()
            .all(|p| super::super::wire::passes_point(&track, *p)));
        for p in &track.locations {
            let nearest = preview
                .route
                .windows(2)
                .map(|w| {
                    let frame = LocalFrame::new(p.gLat, p.gLng);
                    let xy = |(lat, lon): (f64, f64)| {
                        let (x, y) = frame.to_xy(lat, lon);
                        [x, y]
                    };
                    let (q, _) =
                        route_planner::graph::project_to_segment([0.0, 0.0], xy(w[0]), xy(w[1]));
                    q[0].hypot(q[1])
                })
                .fold(f64::INFINITY, f64::min);
            assert!(nearest < 40.0, "preview/generation mismatch: {nearest}");
        }
    }

    // A concave campus with a road at its re-entrant corner: connecting timed
    // observations without preserving this vertex would leave campus.
    type Campus = (RoadGraph, Vec<Vec<(f64, f64)>>, Vec<(f64, f64)>);
    fn concave_campus() -> Campus {
        let coord = |(x, y): (f64, f64)| (39.9 + y / 111132.0, 116.4 + x / 85400.0);
        let fence = vec![
            (0., 0.),
            (240., 0.),
            (240., 100.),
            (100., 100.),
            (100., 240.),
            (0., 240.),
        ]
        .into_iter()
        .map(coord)
        .collect();
        let path: Vec<_> = vec![
            (10., 10.),
            (230., 10.),
            (230., 90.),
            (90., 90.),
            (90., 230.),
            (10., 230.),
        ]
        .into_iter()
        .map(coord)
        .collect();
        let mut net = RoadGraph::new();
        let nodes: Vec<_> = path
            .iter()
            .map(|&(lat, lon)| net.add_node(Coord::new(lon, lat)))
            .collect();
        net.compute_anchor();
        for i in 0..nodes.len() {
            let j = (i + 1) % nodes.len();
            let a = net.to_m(net.node_coord(nodes[i]));
            let b = net.to_m(net.node_coord(nodes[j]));
            let edge = route_planner::graph::EdgeData {
                way_id: i as u64,
                oneway: false,
                highway: "footway".into(),
                maxspeed_kmh: None,
                length_m: (a[0] - b[0]).hypot(a[1] - b[1]),
            };
            net.add_edge(nodes[i], nodes[j], edge.clone());
            net.add_edge(nodes[j], nodes[i], edge);
        }
        net.rebuild_index();
        (net, vec![fence], path)
    }

    #[test]
    fn long_distance_repeats_inside_concave_campus_and_keeps_statistics() {
        let (net, fences, points) = concave_campus();
        let net = apply_fences(&net, &fences);
        for seed in 0..16 {
            // Campus perimeter is 960 m; 5 km must reuse school roads.
            let track = build_road(5000.0, 2000, seed, 123000, &points, &points, &net).unwrap();
            assert!((track.totalDistance - 5000.0).abs() < 0.001);
            assert_eq!(track.totalTime, 2000);
            track.validate_consistency().unwrap();
            assert!(track
                .locations
                .windows(2)
                .all(|w| route_planner::boundary::segment_inside(
                    &net.boundaries,
                    Coord::new(w[0].gLng, w[0].gLat),
                    Coord::new(w[1].gLng, w[1].gLat)
                )));
            assert!(points
                .iter()
                .all(|p| super::super::wire::passes_point(&track, *p)));
            assert_eq!(track.locations.last().unwrap().steps, track.totalSteps);
        }
    }

    #[test]
    fn no_boundary_extra_mileage_reuses_the_same_checkpoint_circuit() {
        let (mut net, _, points) = concave_campus();
        // A long, returnable side road must not be explored to fill the mileage.
        let start = net.graph.node_indices().next().unwrap();
        let far = net.add_node(Coord::new(116.42, 39.9));
        let edge = route_planner::graph::EdgeData {
            way_id: 99,
            oneway: false,
            highway: "footway".into(),
            maxspeed_kmh: None,
            length_m: 1700.,
        };
        net.add_edge(start, far, edge.clone());
        net.add_edge(far, start, edge);
        net.rebuild_index();
        assert!(net.boundaries.is_empty());
        let short = required_route(&net, &points, 2000., 800).unwrap();
        let long = required_route(&net, &points, 5000., 2000).unwrap();
        assert_eq!(short.length_m, long.length_m);
        assert!(long.length_m < 1100.);
        assert_eq!(
            short
                .points
                .iter()
                .map(|p| (p.lat, p.lon, p.s))
                .collect::<Vec<_>>(),
            long.points
                .iter()
                .map(|p| (p.lat, p.lon, p.s))
                .collect::<Vec<_>>()
        );
        let laps = super::super::route_view::laps(&long, 5000.);
        assert!(laps.len() >= 5);
        assert!((laps.iter().sum::<f64>() - 5000.).abs() < 0.001);
        for seed in 0..4 {
            let track = build_road(5000., 2000, seed, 123000, &points, &points, &net).unwrap();
            track.validate_consistency().unwrap();
            assert!((track.totalDistance - 5000.).abs() < 0.001);
            assert_eq!(track.totalTime, 2000);
            assert!(track.locations.iter().all(|p| p.gLng < 116.405));
            assert!(points
                .iter()
                .all(|p| super::super::wire::passes_point(&track, *p)));
        }
    }

    #[test]
    fn supplied_boundary_still_rejects_outside_points() {
        let (net, fences, mut points) = concave_campus();
        let clipped = apply_fences(&net, &fences);
        points.push((40.0, 117.0));
        assert!(build_road(5000., 2000, 1, 0, &points, &points, &clipped).is_err());
    }

    #[test]
    fn preflight_reports_short_distance_and_marks_offroad_point() {
        let (net, fences, points) = concave_campus();
        let net = apply_fences(&net, &fences);
        let cp: Vec<_> = points.iter().map(|&(a, b)| Coord::new(b, a)).collect();
        let check = route_planner::check::inspect(&net, &cp, 100.);
        assert_eq!(check.components, 1);
        assert!(check.minimum_m.unwrap() > 800.);
        assert!(check.error().unwrap().contains("不足"));
        let bad = route_planner::check::inspect(&net, &[Coord::new(117., 40.)], 5000.);
        assert_eq!(bad.issues[0].point, Some(0));
        assert!(bad.issues[0].position.is_some());
    }

    #[test]
    fn required_points_in_disconnected_school_roads_fail() {
        let (mut net, fences, points) = concave_campus();
        net.graph.clear_edges();
        let nodes: Vec<_> = net.graph.node_indices().collect();
        for (a, b) in [(0, 1), (3, 4)] {
            let edge = route_planner::graph::EdgeData {
                way_id: a as u64,
                oneway: false,
                highway: "footway".into(),
                maxspeed_kmh: None,
                length_m: 200.0,
            };
            net.add_edge(nodes[a], nodes[b], edge.clone());
            net.add_edge(nodes[b], nodes[a], edge);
        }
        net.rebuild_index();
        let net = apply_fences(&net, &fences);
        assert!(build_road(5000., 2000, 1, 0, &[points[0], points[3]], &[], &net).is_err());
    }
}
