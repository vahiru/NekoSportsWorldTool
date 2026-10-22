//! 有向路网图：`petgraph` DiGraph + `rstar` 空间索引 + 等距投影。
//!
//! 节点/边坐标以「度」存储（工作系，已由调用方对齐到 BD 工作系），
//! 距离/曲率计算通过本地等距投影（锚点 + `cos(lat)` 缩放）转成米制平面。

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};
use rstar::{RTree, RTreeObject, AABB};

use crate::geo::{met_per_deg_lng, MET_PER_DEG_LAT, MET_PER_DEG_LNG_EQ};

/// 经纬度（度，工作系）。
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Coord {
    pub lon: f64,
    pub lat: f64,
}

impl Coord {
    pub fn new(lon: f64, lat: f64) -> Self {
        Coord { lon, lat }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeData {
    pub coord: Coord,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EdgeData {
    /// OSM way id（同一物理道路切分出的多段共享，用于重复惩罚）。
    pub way_id: u64,
    pub oneway: bool,
    pub highway: String,
    pub maxspeed_kmh: Option<f64>,
    pub length_m: f64,
}

/// rstar 空间索引条目：一条有向边在米制平面的两端点。
/// 存端点而非边索引：切分边时 `remove_edge` 的 swap-remove 会让边索引失效，
/// 端点索引（节点不随切分删除）则可经 `find_edge` 重新定位当前有效边。
#[derive(Clone, Copy)]
struct Seg {
    a: [f64; 2],
    b: [f64; 2],
    from: NodeIndex,
    to: NodeIndex,
}

impl RTreeObject for Seg {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(self.a, self.b)
    }
}

/// 点到线段距离（用于 rstar 邻域查询：返回与查询点距离 ≤ 半径的边）。
impl rstar::PointDistance for Seg {
    fn distance_2(&self, point: &[f64; 2]) -> f64 {
        let (proj, _) = project_to_segment(*point, self.a, self.b);
        let (dx, dy) = (proj[0] - point[0], proj[1] - point[1]);
        dx * dx + dy * dy
    }
}

#[derive(Clone)]
pub struct RoadGraph {
    pub graph: DiGraph<NodeData, EdgeData>,
    /// 投影锚点（度）。
    pub anchor: Coord,
    /// 经度每度对应米数（按锚点纬度校正）。
    pub meter_per_lng: f64,
    /// 建筑外环（度）。
    pub buildings: Vec<Vec<Coord>>,
    /// 校园边界，工作坐标系；非空时禁止平滑/采样切出边界。
    pub boundaries: Vec<Vec<Coord>>,
    index: RTree<Seg>,
}

impl RoadGraph {
    /// Drawing-only spatial query. Routing always retains the complete graph.
    pub fn visible_edges(&self, min: Coord, max: Coord) -> Vec<[Coord; 2]> {
        let a = self.to_m(min);
        let b = self.to_m(max);
        let bounds = AABB::from_corners(
            [a[0].min(b[0]), a[1].min(b[1])],
            [a[0].max(b[0]), a[1].max(b[1])],
        );
        let mut seen = std::collections::HashSet::new();
        self.index
            .locate_in_envelope_intersecting(&bounds)
            .filter_map(|e| {
                let key = (
                    e.from.index().min(e.to.index()),
                    e.from.index().max(e.to.index()),
                );
                seen.insert(key)
                    .then(|| [self.node_coord(e.from), self.node_coord(e.to)])
            })
            .collect()
    }

    pub fn new() -> Self {
        RoadGraph {
            graph: DiGraph::new(),
            anchor: Coord::new(0.0, 0.0),
            meter_per_lng: MET_PER_DEG_LNG_EQ,
            buildings: Vec::new(),
            boundaries: Vec::new(),
            index: RTree::new(),
        }
    }

    /// 度 → 米制平面（相对锚点）。
    pub fn to_m(&self, c: Coord) -> [f64; 2] {
        [
            (c.lon - self.anchor.lon) * self.meter_per_lng,
            (c.lat - self.anchor.lat) * MET_PER_DEG_LAT,
        ]
    }

    /// 米制平面 → 度。
    pub fn to_deg(&self, p: [f64; 2]) -> Coord {
        Coord::new(
            self.anchor.lon + p[0] / self.meter_per_lng,
            self.anchor.lat + p[1] / MET_PER_DEG_LAT,
        )
    }

    pub fn node_coord(&self, n: NodeIndex) -> Coord {
        self.graph[n].coord
    }

    /// 添加节点，返回索引。
    pub fn add_node(&mut self, coord: Coord) -> NodeIndex {
        self.graph.add_node(NodeData { coord })
    }

    /// 添加有向边。
    pub fn add_edge(&mut self, from: NodeIndex, to: NodeIndex, data: EdgeData) -> EdgeIndex {
        self.graph.add_edge(from, to, data)
    }

    /// 设置投影锚点（由所有节点质心计算）并重建米制缩放。
    pub fn compute_anchor(&mut self) {
        let n = self.graph.node_count();
        if n == 0 {
            return;
        }
        let (mut slat, mut slon) = (0.0f64, 0.0f64);
        for node in self.graph.node_indices() {
            let c = self.graph[node].coord;
            slat += c.lat;
            slon += c.lon;
        }
        let (lat, lon) = (slat / n as f64, slon / n as f64);
        self.anchor = Coord::new(lon, lat);
        self.meter_per_lng = met_per_deg_lng(lat);
    }

    /// 重建空间索引（新增节点/边或切分后调用）。
    pub fn rebuild_index(&mut self) {
        let mut items = Vec::with_capacity(self.graph.edge_count());
        for e in self.graph.edge_indices() {
            let (a, b) = self.graph.edge_endpoints(e).unwrap();
            let (pa, pb) = (
                self.to_m(self.graph[a].coord),
                self.to_m(self.graph[b].coord),
            );
            items.push(Seg {
                a: pa,
                b: pb,
                from: a,
                to: b,
            });
        }
        self.index = RTree::bulk_load(items);
    }

    /// 查询离给定米制平面点最近的边：返回 (边, 垂足参数 t∈[0,1], 垂足点, 距离)。
    pub fn nearest_edge(&self, p: [f64; 2]) -> Option<(EdgeIndex, f64, [f64; 2], f64)> {
        let mut radius = 500.0f64;
        let mut best: Option<(EdgeIndex, f64, [f64; 2], f64)> = None;
        while best.is_none() && radius <= 20_000.0 {
            let mut best_d = f64::INFINITY;
            for seg in self.index.locate_within_distance(p, radius * radius) {
                // 跳过已被切分删除的过期条目（端点对应边已不存在）。
                let edge = match self.graph.find_edge(seg.from, seg.to) {
                    Some(e) => e,
                    None => continue,
                };
                let (proj, t) = project_to_segment(p, seg.a, seg.b);
                let d = dist(p, proj);
                if d < best_d {
                    best_d = d;
                    best = Some((edge, t, proj, d));
                }
            }
            if best.is_some() {
                break;
            }
            radius *= 4.0;
        }
        best
    }

    /// 增量插入一条有向边到空间索引（切分边新增子段时调用，避免全量重建）。
    pub fn index_insert_edge(&mut self, from: NodeIndex, to: NodeIndex) {
        let a = self.to_m(self.graph[from].coord);
        let b = self.to_m(self.graph[to].coord);
        self.index.insert(Seg { a, b, from, to });
    }

    /// 所有道路段（度系，双向道路只出现一次），供 UI 绘制/取范围。
    pub fn edges_deg(&self) -> Vec<[Coord; 2]> {
        let mut seen = std::collections::HashSet::with_capacity(self.graph.edge_count());
        self.graph
            .edge_indices()
            .filter_map(|e| {
                let (a, b) = self.graph.edge_endpoints(e)?;
                seen.insert((a.min(b), a.max(b)))
                    .then(|| [self.graph[a].coord, self.graph[b].coord])
            })
            .collect()
    }
}

impl Default for RoadGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "serde")]
mod serde_impl;

/// 点到线段垂足投影，返回 (垂足点, 归一化参数 t)。
pub fn project_to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> ([f64; 2], f64) {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let denom = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if denom <= 0.0 {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / denom).clamp(0.0, 1.0)
    };
    ([a[0] + t * ab[0], a[1] + t * ab[1]], t)
}

pub fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    (dx * dx + dy * dy).sqrt()
}

/// 射线法点是否在多边形内（经纬度按 (lon,lat)=(x,y) 处理，校园尺度足够）。
pub fn point_in_polygon(poly: &[Coord], p: Coord) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let (x, y) = (p.lon, p.lat);
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (poly[i].lon, poly[i].lat);
        let (xj, yj) = (poly[j].lon, poly[j].lat);
        let intersects = ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi) + xi);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}
