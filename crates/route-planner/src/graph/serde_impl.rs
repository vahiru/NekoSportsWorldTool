//! `RoadGraph` serde representation (feature `serde`).
//!
//! Nodes and edges are written as plain lists in index order, so a decoded
//! graph has the same node/edge indices, attributes, anchor and projection as
//! the one written. The spatial index is derived data and is rebuilt on load.
//! For compactness, edge `highway` strings are interned into a table and
//! `way_id`s are delta-coded (consecutive edges usually share a way).
//!
//! Changing this layout requires bumping `crate::load::GRAPH_FORMAT`.

use super::{Coord, EdgeData, NodeData, RoadGraph};
use petgraph::graph::{DiGraph, NodeIndex};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;

type Graph = DiGraph<NodeData, EdgeData>;

/// (from, to, way_id delta, oneway, highway index, maxspeed, length).
type WireEdge = (u32, u32, i64, bool, u32, Option<f64>, f64);

struct Nodes<'a>(&'a Graph);
impl Serialize for Nodes<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.node_weights().map(|n| n.coord))
    }
}

struct Edges<'a> {
    graph: &'a Graph,
    highway: HashMap<&'a str, u32>,
}
impl Serialize for Edges<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut prev_way = 0u64;
        s.collect_seq(self.graph.raw_edges().iter().map(|e| {
            let w = &e.weight;
            let delta = w.way_id.wrapping_sub(prev_way) as i64;
            prev_way = w.way_id;
            let edge: WireEdge = (
                e.source().index() as u32,
                e.target().index() as u32,
                delta,
                w.oneway,
                self.highway[w.highway.as_str()],
                w.maxspeed_kmh,
                w.length_m,
            );
            edge
        }))
    }
}

#[derive(Serialize)]
struct Out<'a> {
    anchor: Coord,
    meter_per_lng: f64,
    nodes: Nodes<'a>,
    highways: Vec<&'a str>,
    edges: Edges<'a>,
    buildings: &'a [Vec<Coord>],
    boundaries: &'a [Vec<Coord>],
}

#[derive(Deserialize)]
struct In {
    anchor: Coord,
    meter_per_lng: f64,
    nodes: Vec<Coord>,
    highways: Vec<String>,
    edges: Vec<WireEdge>,
    buildings: Vec<Vec<Coord>>,
    boundaries: Vec<Vec<Coord>>,
}

impl Serialize for RoadGraph {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut highways = Vec::new();
        let mut highway = HashMap::new();
        for e in self.graph.raw_edges() {
            highway.entry(e.weight.highway.as_str()).or_insert_with(|| {
                highways.push(e.weight.highway.as_str());
                highways.len() as u32 - 1
            });
        }
        Out {
            anchor: self.anchor,
            meter_per_lng: self.meter_per_lng,
            nodes: Nodes(&self.graph),
            highways,
            edges: Edges {
                graph: &self.graph,
                highway,
            },
            buildings: &self.buildings,
            boundaries: &self.boundaries,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for RoadGraph {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = In::deserialize(d)?;
        if !raw.meter_per_lng.is_finite() || raw.meter_per_lng <= 0.0 {
            return Err(D::Error::custom("invalid projection scale"));
        }
        let mut graph = Graph::with_capacity(raw.nodes.len(), raw.edges.len());
        for coord in raw.nodes {
            graph.add_node(NodeData { coord });
        }
        let n = graph.node_count();
        let mut way_id = 0u64;
        for (a, b, delta, oneway, hw, maxspeed_kmh, length_m) in raw.edges {
            let (a, b) = (a as usize, b as usize);
            if a >= n || b >= n {
                return Err(D::Error::custom("edge endpoint out of range"));
            }
            let highway = raw
                .highways
                .get(hw as usize)
                .ok_or_else(|| D::Error::custom("highway index out of range"))?
                .clone();
            way_id = way_id.wrapping_add(delta as u64);
            let data = EdgeData {
                way_id,
                oneway,
                highway,
                maxspeed_kmh,
                length_m,
            };
            graph.add_edge(NodeIndex::new(a), NodeIndex::new(b), data);
        }
        let mut g = RoadGraph {
            graph,
            anchor: raw.anchor,
            meter_per_lng: raw.meter_per_lng,
            buildings: raw.buildings,
            boundaries: raw.boundaries,
            ..RoadGraph::new()
        };
        g.rebuild_index();
        Ok(g)
    }
}
