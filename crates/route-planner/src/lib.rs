//! 真实道路拓扑路由 + 运动学物理仿真库。
//!
//! 三层解耦：
//!   1. 路由层（纯几何/拓扑）：OSM 解析 → 有向图 → 虚拟节点投影切分 → 闭环路线
//!   2. 动力学层（运动学 + 传感器）：曲率限速 / OU 配速 / AR(1) 噪声 + SDF / 生物力学
//!   3. 协议层（调用方适配）：由 `Route` + 速度序列组装最终轨迹
//!
//! 坐标约定：本 crate 内部以「度」存储（调用方已把 OSM 对齐到 BD 工作系），
//! 距离/曲率经本地等距投影转米制平面。

use petgraph::graph::NodeIndex;

pub mod biomech;
pub mod boundary;
pub mod check;
pub mod geo;
pub mod gpx;
pub mod graph;
pub mod kinematics;
pub mod load;
pub mod noise;
pub mod project;
pub mod route;
pub mod sdf;
pub mod smooth;

pub use biomech::{fatigue_from_km, gait, ou_cadence_series, speed_from};
pub use graph::{point_in_polygon, Coord, RoadGraph};
pub use kinematics::{pace_profile, speed_limit_ahead, KinParams};
pub use load::{load_osm, parse_osm};
pub use noise::GpsJitter;
pub use route::RouteOptions;
pub use sdf::Sdf;
pub use smooth::{turn_speed_limit, RoutePoint};

/// 一条规划好的空间路线（度系采样点 + 总长）。
#[derive(Clone, Debug)]
pub struct Route {
    pub points: Vec<RoutePoint>,
    pub length_m: f64,
}

/// 入口：虚拟节点投影 + 打卡点闭环路由 + 曲率平滑。
///
/// `waypoints[0]` 为起点，`waypoints[1..]` 为必经打卡点（按顺序），
/// 路线闭合回起点；调用方重复整条闭环补足里程。
/// 已有 [`check::inspect_full`] 结果时用 [`check::Inspection::route`]，避免重复搜索。
pub fn plan_route(
    net: &RoadGraph,
    waypoints: &[Coord],
    opts: &RouteOptions,
) -> Result<Route, String> {
    if waypoints.is_empty() {
        return Err("无起点".into());
    }
    let mut g = net.clone();
    let vnodes = project::add_virtual_nodes(&mut g, waypoints)?;
    let (start, must) = split_start(&vnodes);
    let path = route::plan_loop(&g, start, &must)?;
    finish_route(&g, &path, opts)
}

/// 虚拟节点序列 → (起点, 去掉与起点重合者后的必经点)。
pub(crate) fn split_start(vnodes: &[NodeIndex]) -> (NodeIndex, Vec<NodeIndex>) {
    let start = vnodes[0];
    let must = vnodes[1..]
        .iter()
        .copied()
        .filter(|n| *n != start)
        .collect();
    (start, must)
}

/// 节点闭环 → 平滑采样路线（有校园边界时保留全部道路拐点，不切角）。
pub(crate) fn finish_route(
    g: &RoadGraph,
    path: &[NodeIndex],
    opts: &RouteOptions,
) -> Result<Route, String> {
    let coords = route::path_coords(g, path);
    let points = if g.boundaries.is_empty() {
        smooth::smooth_and_sample(g, &coords, opts.min_radius_m, opts.turn_threshold_deg, 1.0)
    } else {
        // Keep every road corner: smoothing may cut through a concave school boundary.
        if !coords
            .windows(2)
            .all(|p| boundary::segment_inside(&g.boundaries, p[0], p[1]))
        {
            return Err("路线连线越出学校围栏".into());
        }
        let mut s = 0.0;
        let mut out = Vec::with_capacity(coords.len());
        for (i, c) in coords.iter().enumerate() {
            if i > 0 {
                let a = g.to_m(coords[i - 1]);
                let b = g.to_m(*c);
                s += (b[0] - a[0]).hypot(b[1] - a[1]);
            }
            out.push(RoutePoint {
                lon: c.lon,
                lat: c.lat,
                s,
                curvature: 0.0,
            });
        }
        out
    };
    if points.is_empty() {
        return Err("路线平滑后为空".into());
    }
    let length_m = points.last().map(|p| p.s).unwrap_or(0.0);
    Ok(Route { points, length_m })
}
