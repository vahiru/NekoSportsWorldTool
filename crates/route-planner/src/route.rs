//! 打卡点闭环：起点 → 必经点（按序）→ 起点，逐段最短路拼接。
//!
//! 调用方重复整条闭环补足里程（不再向闭环外的道路游走）。

use petgraph::algo::astar;
use petgraph::graph::NodeIndex;

use crate::graph::{dist, Coord, RoadGraph};

#[derive(Clone, Debug)]
pub struct RouteOptions {
    /// 转弯切角阈值（度）。
    pub turn_threshold_deg: f64,
    /// 最小转弯半径（米）。
    pub min_radius_m: f64,
}

impl Default for RouteOptions {
    fn default() -> Self {
        RouteOptions {
            turn_threshold_deg: 30.0,
            min_radius_m: 6.0,
        }
    }
}

/// 两点最短路径（节点序列，几何长度）。
pub fn shortest_path(g: &RoadGraph, from: NodeIndex, to: NodeIndex) -> Option<Vec<NodeIndex>> {
    let res = astar(
        &g.graph,
        from,
        |n| n == to,
        |e| e.weight().length_m,
        |_| 0.0,
    );
    res.map(|(_cost, path)| path)
}

fn dist_nodes(g: &RoadGraph, a: NodeIndex, b: NodeIndex) -> f64 {
    dist(g.to_m(g.node_coord(a)), g.to_m(g.node_coord(b)))
}

/// 生成闭合路线（节点序列，首尾一致）：起点 → `must_pass`（按序）→ 起点。
///
/// 必经点不可达时直接报错（围栏裁剪可能把路网切成多个连通分量，落在被裁掉
/// 分量上的必经点不得静默丢失）。
pub fn plan_loop(
    g: &RoadGraph,
    start: NodeIndex,
    must_pass: &[NodeIndex],
) -> Result<Vec<NodeIndex>, String> {
    plan_loop_with(g, start, must_pass, |a, b| shortest_path(g, a, b))
}

/// 同 [`plan_loop`]，逐段最短路由 `leg` 提供（可复用调用方已求得的结果）。
pub(crate) fn plan_loop_with(
    g: &RoadGraph,
    start: NodeIndex,
    must_pass: &[NodeIndex],
    mut leg: impl FnMut(NodeIndex, NodeIndex) -> Option<Vec<NodeIndex>>,
) -> Result<Vec<NodeIndex>, String> {
    let mut path = vec![start];
    let mut current = start;
    for &target in must_pass {
        let segment = leg(current, target).ok_or_else(|| {
            format!(
                "必经点 ({:.6},{:.6}) 不可达：可能落在围栏外或路网不连通",
                g.node_coord(target).lat,
                g.node_coord(target).lon
            )
        })?;
        path.extend(segment.into_iter().skip(1));
        current = target;
    }
    let home = leg(current, start).ok_or("点位无法形成可返回起点的道路闭环")?;
    path.extend(home.into_iter().skip(1));
    if path.len() < 2 {
        // One distinct checkpoint still needs a real, returnable road circuit.
        let neighbor = g
            .graph
            .neighbors(start)
            .filter(|n| *n != start)
            .filter_map(|n| shortest_path(g, n, start).map(|home| (n, home)))
            .min_by(|(a, _), (b, _)| dist_nodes(g, start, *a).total_cmp(&dist_nodes(g, start, *b)))
            .ok_or("点位附近没有可返回起点的道路")?;
        path.push(neighbor.0);
        path.extend(neighbor.1.into_iter().skip(1));
    }
    Ok(path)
}

/// 节点路径 → 度系折线坐标（供 UI/平滑使用）。
pub fn path_coords(g: &RoadGraph, path: &[NodeIndex]) -> Vec<Coord> {
    path.iter().map(|&n| g.node_coord(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loop_requires_a_return_path_even_for_a_single_checkpoint() {
        let mut g = RoadGraph::new();
        let a = g.add_node(Coord::new(116.4, 39.9));
        let b = g.add_node(Coord::new(116.401, 39.9));
        g.compute_anchor();
        let edge = crate::graph::EdgeData {
            way_id: 1,
            oneway: true,
            highway: "footway".into(),
            maxspeed_kmh: None,
            length_m: 85.,
        };
        g.add_edge(a, b, edge.clone());
        assert_eq!(
            plan_loop(&g, a, &[b]).unwrap_err(),
            "点位无法形成可返回起点的道路闭环"
        );
        assert!(plan_loop(&g, a, &[]).is_err());
        assert!(plan_loop(&g, b, &[a]).unwrap_err().contains("不可达"));
        g.add_edge(b, a, edge);
        assert_eq!(plan_loop(&g, a, &[]).unwrap(), vec![a, b, a]);
        assert_eq!(plan_loop(&g, a, &[b]).unwrap(), vec![a, b, a]);
    }
}
