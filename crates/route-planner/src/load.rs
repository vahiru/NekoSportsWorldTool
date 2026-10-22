//! OSM `.osm`（XML）解析：节点 / 道路 way / 建筑 way+multipolygon relation。
//!
//! 输入为 openstreetmap.org 导出的标准 OSM XML（`<node>`/`<way>`/`<relation>`）。
//! 只抽取：`highway` 路径（剔除机动车道）、`building` 轮廓。

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::geo::haversine_m;
use crate::graph::{Coord, EdgeData, RoadGraph};

/// 一条道路 way。
pub struct OsmRoad {
    pub id: u64,
    pub node_refs: Vec<u64>,
    pub oneway: bool,
    pub oneway_reversed: bool,
    pub highway: String,
    pub maxspeed_kmh: Option<f64>,
}

/// 一个建筑：外环为节点 id 序列。
pub struct OsmBuilding {
    pub outer: Vec<Vec<u64>>,
}

pub struct ParsedOsm {
    pub nodes: HashMap<u64, Coord>,
    pub roads: Vec<OsmRoad>,
    pub buildings: Vec<OsmBuilding>,
}

/// 不应被当作可跑步道路的 highway 值。
fn is_motorized(h: &str) -> bool {
    matches!(
        h,
        "motorway"
            | "motorway_link"
            | "trunk"
            | "trunk_link"
            | "primary"
            | "primary_link"
            | "secondary"
            | "secondary_link"
            | "construction"
            | "proposed"
    )
}

fn parse_maxspeed(v: &str) -> Option<f64> {
    let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: f64 = digits.parse().ok()?;
    if v.to_ascii_lowercase().contains("mph") {
        Some(n * 1.60934)
    } else {
        Some(n)
    }
}

fn text(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

fn num<T: std::str::FromStr>(v: &[u8]) -> Option<T> {
    std::str::from_utf8(v).ok()?.parse().ok()
}

/// way 解析中只保留后续会用到的标签（与「首个/任一」的原判定语义一致）。
#[derive(Default)]
struct WayTags {
    building: bool,
    highway: Option<String>,
    oneway: bool,
    oneway_reversed: bool,
    /// 首个 maxspeed 标签的解析结果（外层 Some 表示已出现过）。
    maxspeed: Option<Option<f64>>,
}

impl WayTags {
    fn add(&mut self, k: &[u8], v: &[u8]) {
        match k {
            b"building" => self.building |= v != b"no",
            b"highway" if self.highway.is_none() => self.highway = Some(text(v)),
            b"oneway" => {
                self.oneway |= matches!(v, b"yes" | b"true" | b"1");
                self.oneway_reversed |= v == b"-1";
            }
            b"maxspeed" if self.maxspeed.is_none() => {
                self.maxspeed = Some(parse_maxspeed(&String::from_utf8_lossy(v)))
            }
            _ => {}
        }
    }
}

/// 已解析 way 的节点序列所在位置（relation 建筑外环回填时取用，避免逐条克隆）。
enum WayNodes {
    Road(usize),
    Building(usize),
    Other(Vec<u64>),
}

/// 解析 OSM XML 字节流。
pub fn parse_osm(bytes: &[u8]) -> Result<ParsedOsm, String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut nodes: HashMap<u64, Coord> = HashMap::new();
    let mut roads: Vec<OsmRoad> = Vec::new();
    let mut buildings: Vec<OsmBuilding> = Vec::new();

    // 所有 way 的节点序列位置（供 relation 建筑外环回填）。
    let mut way_nodes: HashMap<u64, WayNodes> = HashMap::new();
    // relation 建筑：收集 outer way id。
    let mut rel_buildings: Vec<Vec<u64>> = Vec::new();

    let mut cur_way: Option<(u64, Vec<u64>, WayTags)> = None;
    // relation：outer way 成员 id + 是否为建筑。
    let mut cur_rel: Option<Vec<u64>> = None;
    let mut cur_rel_building = false;

    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                // 单次遍历属性，按原始键名匹配；重复键以最后一次为准。
                let (mut id, mut lat, mut lon, mut r#ref) = (None, None, None, None);
                let (mut k, mut v, mut mtype, mut role) = (None, None, None, None);
                for a in e.attributes().with_checks(false) {
                    let a = a.map_err(|e| format!("属性解析失败: {e}"))?;
                    let val = a.value;
                    match a.key.as_ref() {
                        b"id" => id = Some(val),
                        b"lat" => lat = Some(val),
                        b"lon" => lon = Some(val),
                        b"ref" => r#ref = Some(val),
                        b"k" => k = Some(val),
                        b"v" => v = Some(val),
                        b"type" => mtype = Some(val),
                        b"role" => role = Some(val),
                        _ => {}
                    }
                }
                let id = id.and_then(|v| num::<u64>(&v));
                match e.name().as_ref() {
                    b"node" => {
                        if let (Some(id), Some(lat), Some(lon)) = (
                            id,
                            lat.and_then(|v| num::<f64>(&v)),
                            lon.and_then(|v| num::<f64>(&v)),
                        ) {
                            nodes.insert(id, Coord::new(lon, lat));
                        }
                    }
                    b"way" => {
                        cur_way = Some((id.unwrap_or(0), Vec::new(), WayTags::default()));
                    }
                    b"relation" => {
                        cur_rel = Some(Vec::new());
                        cur_rel_building = false;
                    }
                    b"nd" => {
                        if let Some((_, refs, _)) = cur_way.as_mut() {
                            if let Some(r) = r#ref.and_then(|v| num::<u64>(&v)) {
                                refs.push(r);
                            }
                        }
                    }
                    b"tag" => {
                        let k = k.as_deref().unwrap_or(b"");
                        let v = v.as_deref().unwrap_or(b"");
                        if let Some((_, _, tags)) = cur_way.as_mut() {
                            tags.add(k, v);
                        }
                        if cur_rel.is_some() && k == b"building" && v != b"no" {
                            cur_rel_building = true;
                        }
                    }
                    b"member" => {
                        if let Some(members) = cur_rel.as_mut() {
                            let is_outer_way = mtype.as_deref() == Some(b"way")
                                && role.as_deref() == Some(b"outer");
                            if let Some(r) =
                                r#ref.filter(|_| is_outer_way).and_then(|v| num::<u64>(&v))
                            {
                                members.push(r);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"way" => {
                    if let Some((id, refs, tags)) = cur_way.take() {
                        let slot = if tags.building && refs.len() >= 3 {
                            buildings.push(OsmBuilding { outer: vec![refs] });
                            WayNodes::Building(buildings.len() - 1)
                        } else {
                            match tags.highway {
                                Some(h) if !is_motorized(&h) && refs.len() >= 2 => {
                                    roads.push(OsmRoad {
                                        id,
                                        node_refs: refs,
                                        oneway: tags.oneway,
                                        oneway_reversed: tags.oneway_reversed,
                                        highway: h,
                                        maxspeed_kmh: tags.maxspeed.flatten(),
                                    });
                                    WayNodes::Road(roads.len() - 1)
                                }
                                _ => WayNodes::Other(refs),
                            }
                        };
                        way_nodes.insert(id, slot);
                    }
                }
                b"relation" => {
                    if let Some(outer) = cur_rel.take() {
                        if cur_rel_building && !outer.is_empty() {
                            rel_buildings.push(outer);
                        }
                    }
                    cur_rel_building = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("OSM 解析失败: {e}")),
            _ => {}
        }
    }

    // 回填 relation 建筑外环（outer way 的节点序列）。
    for outer_ways in rel_buildings {
        let mut rings: Vec<Vec<u64>> = Vec::new();
        for wid in outer_ways {
            let refs = match way_nodes.get(&wid) {
                Some(WayNodes::Road(i)) => &roads[*i].node_refs,
                Some(WayNodes::Building(i)) => &buildings[*i].outer[0],
                Some(WayNodes::Other(refs)) => refs,
                None => continue,
            };
            if refs.len() >= 3 {
                rings.push(refs.clone());
            }
        }
        if !rings.is_empty() {
            buildings.push(OsmBuilding { outer: rings });
        }
    }

    Ok(ParsedOsm {
        nodes,
        roads,
        buildings,
    })
}

/// 用道路 way 构建路网图。
pub fn build_graph(parsed: &ParsedOsm) -> RoadGraph {
    let mut g = RoadGraph::new();
    let mut node_map: HashMap<u64, petgraph::graph::NodeIndex> = HashMap::new();

    for r in &parsed.roads {
        // 缺失节点（如按 bbox 裁剪导出）处断开：只连接原序列中相邻且都存在的节点，
        // 不跨缺口生成直线捷径边。
        let mut prev: Option<(petgraph::graph::NodeIndex, Coord)> = None;
        for id in &r.node_refs {
            let Some(&c) = parsed.nodes.get(id) else {
                prev = None;
                continue;
            };
            let next = *node_map.entry(*id).or_insert_with(|| g.add_node(c));
            if let Some((p, a)) = prev {
                let len = haversine_m(a, c);
                let edge = |oneway| EdgeData {
                    way_id: r.id,
                    oneway,
                    highway: r.highway.clone(),
                    maxspeed_kmh: r.maxspeed_kmh,
                    length_m: len,
                };
                if !r.oneway_reversed {
                    g.add_edge(p, next, edge(r.oneway));
                }
                if !r.oneway {
                    g.add_edge(next, p, edge(r.oneway));
                }
            }
            prev = Some((next, c));
        }
    }

    let mut bld: Vec<Vec<Coord>> = Vec::new();
    for b in &parsed.buildings {
        for ring in &b.outer {
            let coords: Vec<Coord> = ring
                .iter()
                .filter_map(|id| parsed.nodes.get(id).copied())
                .collect();
            if coords.len() >= 3 {
                bld.push(coords);
            }
        }
    }
    g.buildings = bld;

    g.compute_anchor();
    g.rebuild_index();
    g
}

/// 完整流程：解析 OSM → 构建路网。
pub fn load_osm(bytes: &[u8]) -> Result<RoadGraph, String> {
    let parsed = parse_osm(bytes)?;
    if parsed.roads.is_empty() {
        return Err("OSM 中未找到可用的 highway 道路".into());
    }
    Ok(build_graph(&parsed))
}
