//! `RoadGraph` serde round trip: a decoded graph must be indistinguishable
//! from the freshly parsed one (indices, attributes, index-backed queries and
//! planning results).
#![cfg(feature = "serde")]

use route_planner::{check, plan_route, Coord, RoadGraph, RouteOptions};

/// Synthetic OSM grid: two-way, one-way and reversed one-way streets, speed
/// limits, a way broken by a missing node, and a building ring.
fn grid_xml(n: usize) -> String {
    let mut xml = String::from("<osm version=\"0.6\">\n");
    let id = |i: usize, j: usize| 1 + j * n + i;
    for j in 0..n {
        for i in 0..n {
            xml += &format!(
                "<node id=\"{}\" lat=\"{:.7}\" lon=\"{:.7}\"/>\n",
                id(i, j),
                39.9 + j as f64 * 0.0005,
                116.4 + i as f64 * 0.0005
            );
        }
    }
    let mut way = 1;
    for j in 0..n {
        let refs: String = (0..n)
            .map(|i| format!("<nd ref=\"{}\"/>", id(i, j)))
            .collect();
        let tags = match j % 3 {
            0 => "<tag k=\"highway\" v=\"residential\"/><tag k=\"maxspeed\" v=\"30\"/>",
            1 => "<tag k=\"highway\" v=\"service\"/><tag k=\"oneway\" v=\"yes\"/>",
            _ => "<tag k=\"highway\" v=\"footway\"/><tag k=\"oneway\" v=\"-1\"/>",
        };
        xml += &format!("<way id=\"{way}\">{refs}{tags}</way>\n");
        way += 1;
    }
    for i in 0..n {
        let mut refs: String = (0..n)
            .map(|j| format!("<nd ref=\"{}\"/>", id(i, j)))
            .collect();
        if i == 1 {
            refs += "<nd ref=\"999999999\"/>";
        }
        xml += &format!("<way id=\"{way}\">{refs}<tag k=\"highway\" v=\"path\"/></way>\n");
        way += 1;
    }
    xml += &format!(
        "<way id=\"{way}\"><nd ref=\"{}\"/><nd ref=\"{}\"/><nd ref=\"{}\"/><nd ref=\"{}\"/><tag k=\"building\" v=\"yes\"/></way>\n",
        id(0, 0),
        id(1, 0),
        id(1, 1),
        id(0, 0)
    );
    xml + "</osm>\n"
}

fn round_trip(g: &RoadGraph) -> RoadGraph {
    postcard::from_bytes(&postcard::to_stdvec(g).unwrap()).unwrap()
}

fn assert_same(a: &RoadGraph, b: &RoadGraph) {
    let nodes = |g: &RoadGraph| g.graph.node_weights().cloned().collect::<Vec<_>>();
    let edges = |g: &RoadGraph| {
        g.graph
            .raw_edges()
            .iter()
            .map(|e| (e.source(), e.target(), e.weight.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(nodes(a), nodes(b));
    assert_eq!(edges(a), edges(b));
    assert_eq!(a.anchor, b.anchor);
    assert_eq!(a.meter_per_lng.to_bits(), b.meter_per_lng.to_bits());
    assert_eq!(a.buildings, b.buildings);
    assert_eq!(a.boundaries, b.boundaries);
}

#[test]
fn parsed_graph_round_trips_exactly() {
    let mut parsed = route_planner::load_osm(grid_xml(8).as_bytes()).unwrap();
    assert!(parsed.graph.node_count() > 50 && parsed.graph.edge_count() > 100);
    assert!(!parsed.buildings.is_empty());
    let b = |lon, lat| Coord::new(lon, lat);
    parsed.boundaries = vec![vec![
        b(116.39, 39.89),
        b(116.41, 39.89),
        b(116.41, 39.91),
        b(116.39, 39.91),
    ]];
    let decoded = round_trip(&parsed);
    assert_same(&parsed, &decoded);

    // Index-backed queries see the rebuilt spatial index.
    let (min, max) = (b(116.4, 39.9), b(116.4015, 39.9015));
    assert_eq!(
        parsed.visible_edges(min, max).len(),
        decoded.visible_edges(min, max).len()
    );
    let p = parsed.to_m(b(116.40112, 39.90131));
    assert_eq!(parsed.nearest_edge(p), decoded.nearest_edge(p));

    let wps = [
        b(116.40002, 39.9),
        b(116.4031, 39.9025),
        b(116.4012, 39.9034),
    ];
    let opts = RouteOptions::default();
    let key = |r: route_planner::Route| -> Vec<(u64, u64, u64)> {
        r.points
            .iter()
            .map(|p| (p.lon.to_bits(), p.lat.to_bits(), p.s.to_bits()))
            .collect()
    };
    parsed.boundaries.clear();
    let decoded = round_trip(&parsed);
    assert_eq!(
        key(plan_route(&parsed, &wps, &opts).unwrap()),
        key(plan_route(&decoded, &wps, &opts).unwrap())
    );
    let (x, y) = (
        check::inspect_full(&parsed, &wps, 1000.0),
        check::inspect_full(&decoded, &wps, 1000.0),
    );
    assert_eq!(x.check.minimum_m, y.check.minimum_m);
    assert_eq!(key(x.route(&opts).unwrap()), key(y.route(&opts).unwrap()));
}

#[test]
fn corrupt_graph_bytes_are_rejected() {
    let g = route_planner::load_osm(grid_xml(4).as_bytes()).unwrap();
    let bytes = postcard::to_stdvec(&g).unwrap();
    assert!(postcard::from_bytes::<RoadGraph>(&bytes[..bytes.len() / 2]).is_err());
    // Out-of-range node / highway references must not panic or yield a graph.
    let mut tiny = RoadGraph::new();
    tiny.meter_per_lng = 1.0;
    let a = tiny.add_node(Coord::new(0.0, 0.0));
    let b = tiny.add_node(Coord::new(0.0, 0.001));
    let edge = route_planner::graph::EdgeData {
        way_id: 7,
        oneway: false,
        highway: "x".into(),
        maxspeed_kmh: None,
        length_m: 111.0,
    };
    tiny.add_edge(a, b, edge);
    let bytes = postcard::to_stdvec(&tiny).unwrap();
    assert_same(&tiny, &postcard::from_bytes(&bytes).unwrap());
    // Layout: anchor(16) scale(8) node count(1) nodes(32) highway table(1+1+1)
    // edge count(1), then the edge: from(1) to(1) way delta(1) oneway(1) highway(1)
    let edge_at = 16 + 8 + 1 + 32 + 3 + 1;
    for (offset, value) in [(0, 9), (1, 2), (4, 1)] {
        let mut bad = bytes.clone();
        bad[edge_at + offset] = value;
        assert!(postcard::from_bytes::<RoadGraph>(&bad).is_err(), "{offset}");
    }
}

/// Release timing of XML parse vs binary decode on a ~20k-node graph
/// (`GRID_SIDE` env var overrides the 142×142 grid):
/// `cargo test --release -p route-planner --features serde --test serde -- --ignored --nocapture`
#[test]
#[ignore]
fn timing_parse_vs_decode() {
    let side = std::env::var("GRID_SIDE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(142);
    let xml = grid_xml(side);
    let t = std::time::Instant::now();
    let g = route_planner::load_osm(xml.as_bytes()).unwrap();
    let parse = t.elapsed();
    let bytes = postcard::to_stdvec(&g).unwrap();
    let t = std::time::Instant::now();
    let decoded: RoadGraph = postcard::from_bytes(&bytes).unwrap();
    let decode = t.elapsed();
    assert_same(&g, &decoded);
    println!(
        "nodes={} edges={} xml={} KiB bin={} KiB parse={parse:?} decode={decode:?}",
        g.graph.node_count(),
        g.graph.edge_count(),
        xml.len() / 1024,
        bytes.len() / 1024
    );
}
