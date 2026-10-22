//! School-scoped public OSM download. No account credentials are sent to Overpass.
use route_planner::RoadGraph;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Duration};

const MAX_BYTES: u64 = 24 * 1024 * 1024;
const TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const ENDPOINT: &str = "https://overpass-api.de/api/interpreter";

pub fn validate_fences(fences: &[Vec<(f64, f64)>]) -> Result<(), String> {
    if fences.is_empty() {
        return Err("当前学校没有有效围栏，无法保证校内路线；请先确认学校围栏".into());
    }
    for ring in fences {
        if ring.len() < 3
            || ring.iter().any(|&(lat, lon)| {
                !lat.is_finite() || !lon.is_finite() || lat.abs() > 85.0 || lon.abs() > 180.0
            })
        {
            return Err("学校围栏坐标无效".into());
        }
        let (oy, ox) = ring[0];
        let area: f64 = ring
            .iter()
            .zip(ring.iter().cycle().skip(1))
            .map(|(&(y, x), &(v, u))| (x - ox) * (v - oy) - (u - ox) * (y - oy))
            .sum();
        if area.abs() < 1e-12 {
            return Err("学校围栏面积无效".into());
        }
    }
    Ok(())
}

// Invert the same forward transform used to align roads, avoiding mixed coordinate systems.
pub fn to_wgs(lat: f64, lon: f64) -> (f64, f64) {
    let (mut y, mut x) = (lat, lon);
    for _ in 0..16 {
        let (by, bx) = crate::track::geom::wgs84_to_bd09(y, x);
        y += lat - by;
        x += lon - bx;
    }
    (y, x)
}

fn query(fences: &[Vec<(f64, f64)>]) -> Result<String, String> {
    validate_fences(fences)?;
    query_coordinates(&fences.iter().flatten().copied().collect::<Vec<_>>(), 0.001)
}

fn query_coordinates(points: &[(f64, f64)], margin: f64) -> Result<String, String> {
    if points.is_empty()
        || points
            .iter()
            .any(|&(a, b)| !a.is_finite() || !b.is_finite() || a.abs() > 85. || b.abs() > 180.)
    {
        return Err("缺少有效点位，不能确定路网下载范围".into());
    }
    let (mut south, mut west, mut north, mut east) = (90f64, 180f64, -90f64, -180f64);
    for &(lat, lon) in points {
        let (y, x) = to_wgs(lat, lon);
        south = south.min(y);
        north = north.max(y);
        west = west.min(x);
        east = east.max(x);
    }
    // Reject unexpectedly huge / geographically separate school areas before downloading.
    if north - south > 0.15 || east - west > 0.2 {
        return Err("路网下载范围跨度过大，请确认当前学校和点位后重试".into());
    }
    let bbox = format!(
        "{:.7},{:.7},{:.7},{:.7}",
        south - margin,
        west - margin,
        north + margin,
        east + margin
    );
    Ok(format!("[out:xml][timeout:25];(way[highway]({bbox});way[building]({bbox});relation[building]({bbox}););(._;>;);out body;"))
}

fn parse(bytes: &[u8]) -> Result<RoadGraph, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("学校路网响应超过 24 MiB".into());
    }
    let xml = std::str::from_utf8(bytes).map_err(|_| "学校路网不是有效 XML")?;
    if !xml.contains("<osm") || !xml.contains("</osm>") || xml.contains("<remark") {
        return Err("路网服务返回错误或不完整数据，请稍后重试".into());
    }
    let net = route_planner::load_osm(bytes)?;
    check_graph(&net)?;
    Ok(net)
}

/// Shared by XML parsing and cached binaries, so a decoded graph is held to the
/// same standard as a freshly parsed one.
fn check_graph(net: &RoadGraph) -> Result<(), String> {
    if net.graph.edge_count() == 0
        || net.graph.node_weights().any(|n| {
            !n.coord.lon.is_finite()
                || !n.coord.lat.is_finite()
                || n.coord.lon.abs() > 180.0
                || n.coord.lat.abs() > 90.0
        })
    {
        return Err("当前学校没有可用的 OSM 道路数据，可手动导入完整路网".into());
    }
    Ok(())
}

fn download(query: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(45))
        .build();
    let response = agent
        .post(ENDPOINT)
        .set("User-Agent", "NekoSportsWorldTool/0.3.0 (school-road-map)")
        .send_form(&[("data", query)])
        .map_err(|e| format!("自动获取学校路网失败：{e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取学校路网失败：{e}"))?;
    Ok(bytes)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Cache {
    key: String,
    loaded_ms: i64,
    xml: String,
    #[serde(default)]
    school: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    fences: Vec<Vec<(f64, f64)>>,
    #[serde(default)]
    points: Vec<(f64, f64)>,
}

// On-disk layout of one cache entry in the cache directory (`key` = 64 hex chars):
//
// - `{key}.json`      the record above: raw OSM XML + context. Source of truth;
//                     its format is unchanged so older builds keep reading it.
// - `{key}.graph`     postcard `GraphHeader` + the parsed raw WGS `RoadGraph`
//                     (before alignment / fence clipping, so both `Output`s use it).
// - `{key}.meta.json` `Meta`: exactly what the cache list shows, plus the record's
//                     length/mtime and the XML hash used to validate the binary.
//
// The record is always written first; sidecars are derived from it, written
// atomically, and rebuilt lazily from the XML whenever they are missing, stale
// (record rewritten by anyone else, e.g. an older build) or undecodable.
const GRAPH_SUFFIX: &str = ".graph";
const META_SUFFIX: &str = ".meta.json";
/// Layout version of the `.graph` / `.meta.json` sidecars themselves. Parser
/// semantics are versioned by `route_planner::load::GRAPH_FORMAT` (bump that one
/// when parsing changes); both are checked before a cached graph is used.
const SIDECAR_VERSION: u32 = 1;
const GRAPH_MAGIC: [u8; 8] = *b"NSWROADS";

#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct GraphHeader {
    magic: [u8; 8],
    sidecar: u32,
    graph_format: u32,
    key: String,
    xml_sha256: String,
}

impl GraphHeader {
    fn new(key: &str, xml_sha256: &str) -> Self {
        GraphHeader {
            magic: GRAPH_MAGIC,
            sidecar: SIDECAR_VERSION,
            graph_format: route_planner::load::GRAPH_FORMAT,
            key: key.into(),
            xml_sha256: xml_sha256.into(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Meta {
    version: u32,
    key: String,
    school: String,
    name: String,
    loaded_ms: i64,
    fences: Vec<Vec<(f64, f64)>>,
    points: Vec<(f64, f64)>,
    xml_sha256: String,
    /// The record file this sidecar describes; any other length/mtime means the
    /// record was rewritten without us and the sidecar is ignored.
    record_len: u64,
    record_mtime_ns: Option<u64>,
}

/// (length, mtime in ns) of a record file.
type Stamp = (u64, Option<u64>);

fn stamp(m: &std::fs::Metadata) -> Stamp {
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64);
    (m.len(), mtime)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_key(key: &str) -> Result<(), String> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("无效缓存标识".into());
    }
    Ok(())
}

fn record_path(dir: &Path, key: &str) -> Result<std::path::PathBuf, String> {
    valid_key(key)?;
    Ok(dir.join(format!("{key}.json")))
}

fn sidecar_paths(dir: &Path, key: &str) -> [std::path::PathBuf; 2] {
    [
        dir.join(format!("{key}{GRAPH_SUFFIX}")),
        dir.join(format!("{key}{META_SUFFIX}")),
    ]
}

/// Same-directory temp file → fsync → rename, so readers only ever see a whole
/// old or whole new file. Returns the written file's metadata (the rename keeps
/// it), which stamps sidecars without racing a concurrent rewrite.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<std::fs::Metadata> {
    use std::io::Write;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("cache");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let meta = file.metadata()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(meta)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The record and the stamp of exactly the bytes that were read.
fn read_record(dir: &Path, key: &str) -> Result<(Cache, Stamp), String> {
    let mut file = std::fs::File::open(record_path(dir, key)?).map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES * 2 {
        return Err("缓存过大".into());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let cache: Cache = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if cache.key != key {
        return Err("缓存标识不一致".into());
    }
    Ok((cache, stamp(&meta)))
}

fn meta_of(cache: &Cache, xml_sha256: String, (record_len, record_mtime_ns): Stamp) -> Meta {
    Meta {
        version: SIDECAR_VERSION,
        key: cache.key.clone(),
        school: cache.school.clone(),
        name: cache.name.clone(),
        loaded_ms: cache.loaded_ms,
        fences: cache.fences.clone(),
        points: cache.points.clone(),
        xml_sha256,
        record_len,
        record_mtime_ns,
    }
}

/// Sidecar metadata, only while it still describes the record on disk.
fn read_meta(dir: &Path, key: &str) -> Option<Meta> {
    let record = std::fs::metadata(record_path(dir, key).ok()?).ok()?;
    let [_, path] = sidecar_paths(dir, key);
    if std::fs::metadata(&path).ok()?.len() > MAX_BYTES {
        return None;
    }
    let meta: Meta = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (meta.version == SIDECAR_VERSION
        && meta.key == key
        && (meta.record_len, meta.record_mtime_ns) == stamp(&record))
    .then_some(meta)
}

fn encode_graph(header: &GraphHeader, net: &RoadGraph) -> Result<Vec<u8>, String> {
    let bytes = postcard::to_stdvec(header).map_err(|e| e.to_string())?;
    postcard::to_extend(net, bytes).map_err(|e| e.to_string())
}

/// The cached binary graph, if it was built by this parser from the XML that
/// `meta` describes; anything else (missing, corrupt, stale) is `None`.
fn read_graph(dir: &Path, meta: &Meta) -> Option<RoadGraph> {
    let [path, _] = sidecar_paths(dir, &meta.key);
    if std::fs::metadata(&path).ok()?.len() > MAX_BYTES * 2 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let (header, rest) = postcard::take_from_bytes::<GraphHeader>(&bytes).ok()?;
    if header != GraphHeader::new(&meta.key, &meta.xml_sha256) {
        return None;
    }
    let net: RoadGraph = postcard::from_bytes(rest).ok()?;
    check_graph(&net).ok()?;
    Some(net)
}

/// Best effort: the cache stays usable through the XML record without sidecars.
fn store_sidecars(dir: &Path, meta: &Meta, graph: Option<&[u8]>) -> Result<(), String> {
    let [graph_path, meta_path] = sidecar_paths(dir, &meta.key);
    let mut result = Ok(());
    if let Some(bytes) = graph {
        result = write_atomic(&graph_path, bytes).map(drop);
    }
    let json = serde_json::to_vec(meta).map_err(|e| e.to_string())?;
    result
        .and(write_atomic(&meta_path, &json).map(drop))
        .map_err(|e| e.to_string())
}

/// Raw cached graph and its metadata. `fresh_at` enforces the validity window.
/// Fast path: metadata + binary graph, never touching the XML. Otherwise (old
/// cache, stale/corrupt sidecars) parse the record and rewrite the sidecars.
fn load_entry(
    dir: &Path,
    key: &str,
    fresh_at: Option<i64>,
    log: &mut dyn FnMut(&str),
) -> Result<(RoadGraph, Meta), String> {
    let expired =
        |loaded_ms: i64| fresh_at.is_some_and(|now| !(0..TTL_MS).contains(&(now - loaded_ms)));
    if let Some(meta) = read_meta(dir, key) {
        if expired(meta.loaded_ms) {
            return Err("路网缓存已过期".into());
        }
        if let Some(net) = read_graph(dir, &meta) {
            return Ok((net, meta));
        }
    }
    let (cache, record) = read_record(dir, key)?;
    if expired(cache.loaded_ms) {
        return Err("路网缓存已过期".into());
    }
    let net = parse(cache.xml.as_bytes())?;
    let meta = meta_of(&cache, sha256_hex(cache.xml.as_bytes()), record);
    let saved = encode_graph(&GraphHeader::new(key, &meta.xml_sha256), &net)
        .and_then(|bytes| store_sidecars(dir, &meta, Some(&bytes)));
    if let Err(e) = saved {
        log(&format!("[road] 路网缓存索引未更新：{e}"));
    }
    Ok((net, meta))
}

#[cfg(test)]
fn cached_network(
    dir: &Path,
    school: &str,
    fences: &[Vec<(f64, f64)>],
    refresh: bool,
    now: i64,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>,
    log: &mut dyn FnMut(&str),
) -> Result<RoadGraph, String> {
    cached_route_network(dir, school, fences, &[], refresh, now, fetch, log)
}

/// 对齐到 BD 系并按围栏裁剪（路由输入）；无围栏时不复制整图。
fn routable(mut net: RoadGraph, fences: &[Vec<(f64, f64)>]) -> RoadGraph {
    crate::track::generate_road::align_network(&mut net);
    if fences.is_empty() {
        net
    } else {
        crate::track::generate_road::apply_fences(&net, fences)
    }
}

/// Which graph the loader returns: the raw WGS graph (preview/UI) or the
/// aligned, fence-filtered routing graph (submission flow).
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
enum Output {
    Raw,
    Routable,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn cached_route_network(
    dir: &Path,
    school: &str,
    fences: &[Vec<(f64, f64)>],
    points: &[(f64, f64)],
    refresh: bool,
    now: i64,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>,
    log: &mut dyn FnMut(&str),
) -> Result<RoadGraph, String> {
    cached_route_network_as(
        dir,
        school,
        fences,
        points,
        refresh,
        now,
        fetch,
        log,
        Output::Raw,
    )
}

#[allow(clippy::too_many_arguments)]
fn cached_route_network_as(
    dir: &Path,
    school: &str,
    fences: &[Vec<(f64, f64)>],
    points: &[(f64, f64)],
    refresh: bool,
    now: i64,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>,
    log: &mut dyn FnMut(&str),
    output: Output,
) -> Result<RoadGraph, String> {
    if school.trim().is_empty() {
        return Err("登录信息缺少学校编号，请重新登录".into());
    }
    let q = if fences.is_empty() {
        query_coordinates(points, 0.002)?
    } else {
        query(fences)?
    };
    let key = route_scope_key(school, fences, points);
    let file = record_path(dir, &key)?;
    if !refresh {
        if let Ok((net, _)) = load_entry(dir, &key, Some(now), log) {
            log("[road] 使用当前学校及下载范围对应的路网缓存");
            return Ok(match output {
                Output::Raw => net,
                Output::Routable => routable(net, fences),
            });
        }
    }
    log("[road] 正在下载当前学校区域的 OpenStreetMap 路网…");
    let bytes = fetch(&q)?;
    let net = parse(&bytes)?;
    let xml_sha256 = sha256_hex(&bytes);
    // Encode the raw graph before the routing path consumes it.
    let graph_bytes = encode_graph(&GraphHeader::new(&key, &xml_sha256), &net);
    // Reject empty in-school coverage before accepting a download into the cache.
    // The routing caller reuses this processed graph; only the raw caller pays a copy.
    let (net, inside_edges) = match output {
        Output::Routable => {
            let net = routable(net, fences);
            let edges = net.graph.edge_count();
            (net, edges)
        }
        Output::Raw => {
            let edges = routable(net.clone(), fences).graph.edge_count();
            (net, edges)
        }
    };
    if inside_edges == 0 {
        return Err("下载的路网在学校围栏内没有可用道路，不能向校外补足距离".into());
    }
    let cache = Cache {
        key,
        loaded_ms: now,
        xml: String::from_utf8(bytes).map_err(|e| e.to_string())?,
        school: school.into(),
        name: String::new(),
        fences: fences.to_vec(),
        points: points.to_vec(),
    };
    // Cache is optional: read-only installation directories must not block a valid download.
    let saved = (|| -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        // Record first: sidecars are stamped with the record they describe.
        let record = write_atomic(
            &file,
            &serde_json::to_vec(&cache).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let meta = meta_of(&cache, xml_sha256, stamp(&record));
        store_sidecars(dir, &meta, graph_bytes.as_deref().ok())
    })();
    if let Err(e) = saved {
        log(&format!("[road] 本次路网可用，但缓存未保存：{e}"));
    }
    Ok(net)
}

#[cfg(any(feature = "gui", test))]
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    // 只由 GUI 读取（匹配当前学校）。
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub school: String,
    pub name: String,
    pub loaded_ms: i64,
    pub bytes: u64,
    pub fences: Vec<Vec<(f64, f64)>>,
    pub points: Vec<(f64, f64)>,
}
pub fn scope_key(school: &str, fences: &[Vec<(f64, f64)>]) -> String {
    // About centimetre precision: stable across JSON floating-point round trips.
    let normalized: Vec<Vec<(i64, i64)>> = fences
        .iter()
        .map(|f| {
            f.iter()
                .map(|&(a, b)| ((a * 1e7).round() as i64, (b * 1e7).round() as i64))
                .collect()
        })
        .collect();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(school, normalized)).unwrap())
    )
}

fn route_scope_key(school: &str, fences: &[Vec<(f64, f64)>], points: &[(f64, f64)]) -> String {
    if !fences.is_empty() {
        return scope_key(school, fences);
    }
    let normalized: Vec<_> = points
        .iter()
        .map(|&(a, b)| ((a * 1e7).round() as i64, (b * 1e7).round() as i64))
        .collect();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&("points-v1", school, normalized)).unwrap())
    )
}
fn cache_dir() -> std::path::PathBuf {
    crate::platform::data_dir().join("school_roads")
}
/// Cache list from the metadata sidecars only. Entries without a valid sidecar
/// (old caches, records rewritten by older builds) are read once from the
/// record and get a sidecar for next time.
#[cfg(any(feature = "gui", test))]
pub fn entries() -> Vec<Entry> {
    entries_in(&cache_dir())
}
#[cfg(any(feature = "gui", test))]
fn entries_in(dir: &Path) -> Vec<Entry> {
    let Ok(files) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for f in files.flatten() {
        let name = f.file_name();
        // Only records (`{key}.json`); sidecars and temp files fail the key check.
        let Some(key) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
            continue;
        };
        if valid_key(key).is_err() {
            continue;
        }
        if let Some(meta) = read_meta(dir, key) {
            out.push(Entry::from(meta));
        } else if let Ok((cache, record)) = read_record(dir, key) {
            let meta = meta_of(&cache, sha256_hex(cache.xml.as_bytes()), record);
            let _ = store_sidecars(dir, &meta, None);
            out.push(Entry::from(meta));
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.loaded_ms));
    out
}
#[cfg(any(feature = "gui", test))]
impl From<Meta> for Entry {
    fn from(m: Meta) -> Self {
        Entry {
            key: m.key,
            school: m.school,
            name: m.name,
            loaded_ms: m.loaded_ms,
            bytes: m.record_len,
            fences: m.fences,
            points: m.points,
        }
    }
}
/// A saved map for offline viewing (no validity window).
#[cfg(any(feature = "gui", test))]
pub fn read_cache(key: &str) -> Result<(RoadGraph, Entry), String> {
    let (net, meta) = load_entry(&cache_dir(), key, None, &mut |_| {})?;
    Ok((net, meta.into()))
}
/// Removes the record and its sidecars.
#[cfg(any(feature = "gui", test))]
pub fn clear_cache(key: &str) -> Result<(), String> {
    let dir = cache_dir();
    let record = record_path(&dir, key)?;
    for path in sidecar_paths(&dir, key) {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
    }
    std::fs::remove_file(record).map_err(|e| e.to_string())
}
/// Labels a downloaded cache. Called on every preview, so an unchanged label is
/// detected from the sidecar without reading or rewriting the large record.
#[cfg(any(feature = "gui", test))]
pub fn save_context(school: &str, name: &str, fences: &[Vec<(f64, f64)>], points: &[(f64, f64)]) {
    let dir = cache_dir();
    let key = route_scope_key(school, fences, points);
    let Ok(path) = record_path(&dir, &key) else {
        return;
    };
    let known = read_meta(&dir, &key);
    if known
        .as_ref()
        .is_some_and(|m| m.name == name && m.points == points)
    {
        return;
    }
    let Ok((mut cache, read)) = read_record(&dir, &key) else {
        return;
    };
    cache.name = name.into();
    cache.points = points.to_vec();
    let Ok(bytes) = serde_json::to_vec(&cache) else {
        return;
    };
    // The XML is unchanged, so an existing binary graph stays valid.
    if let Ok(record) = write_atomic(&path, &bytes) {
        let sha = known
            .filter(|m| (m.record_len, m.record_mtime_ns) == read)
            .map(|m| m.xml_sha256)
            .unwrap_or_else(|| sha256_hex(cache.xml.as_bytes()));
        let _ = store_sidecars(&dir, &meta_of(&cache, sha, stamp(&record)), None);
    }
}

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn load_for_school_cancellable(
    path: &str,
    school: &str,
    fences: &[Vec<(f64, f64)>],
    refresh: bool,
    control: &crate::task_control::Control,
    log: &mut dyn FnMut(&str),
) -> Result<RoadGraph, String> {
    load_cancellable(
        path,
        school,
        fences,
        None,
        refresh,
        control,
        log,
        Output::Raw,
    )
}

/// With no server boundary, download near real school checkpoints. This extent
/// is only a public-map query, never a campus fence or a route constraint.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn load_for_route_cancellable(
    path: &str,
    school: &str,
    fences: &[Vec<(f64, f64)>],
    points: &[(f64, f64)],
    refresh: bool,
    control: &crate::task_control::Control,
    log: &mut dyn FnMut(&str),
) -> Result<RoadGraph, String> {
    load_cancellable(
        path,
        school,
        fences,
        Some(points),
        refresh,
        control,
        log,
        Output::Raw,
    )
}

/// Same sources as `load_for_route_cancellable`, but returns the graph already
/// aligned to BD-09 and fence-filtered, ready for routing.
pub fn load_routable_cancellable(
    path: &str,
    school: &str,
    fences: &[Vec<(f64, f64)>],
    points: &[(f64, f64)],
    refresh: bool,
    control: &crate::task_control::Control,
    log: &mut dyn FnMut(&str),
) -> Result<RoadGraph, String> {
    load_cancellable(
        path,
        school,
        fences,
        Some(points),
        refresh,
        control,
        log,
        Output::Routable,
    )
}

/// `points: None` requires a school fence; `Some` falls back to a point-scoped
/// download when the school has no fence.
#[allow(clippy::too_many_arguments)]
fn load_cancellable(
    path: &str,
    school: &str,
    fences: &[Vec<(f64, f64)>],
    points: Option<&[(f64, f64)]>,
    refresh: bool,
    control: &crate::task_control::Control,
    log: &mut dyn FnMut(&str),
    output: Output,
) -> Result<RoadGraph, String> {
    control.check()?;
    if !path.trim().is_empty() {
        let net = crate::track::generate_road::load_network_path(path)?;
        control.check()?;
        return Ok(match output {
            Output::Raw => net,
            Output::Routable => routable(net, fences),
        });
    }
    let points = match points {
        Some(points) if fences.is_empty() => {
            log("[road] 无学校围栏：按当前学校真实点位获取附近道路；下载范围不是学校边界");
            points
        }
        _ => {
            validate_fences(fences)?;
            &[]
        }
    };
    cached_route_network_as(
        &cache_dir(),
        school,
        fences,
        points,
        refresh,
        crate::crypto::envelope::now_ms(),
        |q| {
            control.check()?;
            let bytes = download(q)?;
            control.check()?;
            Ok(bytes)
        },
        log,
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Vec<Vec<(f64, f64)>>, Vec<u8>) {
        let fences = vec![vec![
            (39.899, 116.399),
            (39.899, 116.403),
            (39.903, 116.403),
            (39.903, 116.399),
        ]
        .into_iter()
        .map(|(a, b)| crate::track::geom::wgs84_to_bd09(a, b))
        .collect()];
        let xml=br#"<osm version="0.6"><node id="1" lat="39.9" lon="116.4"/><node id="2" lat="39.901" lon="116.401"/><way id="1"><nd ref="1"/><nd ref="2"/><tag k="highway" v="footway"/></way></osm>"#.to_vec();
        (fences, xml)
    }
    #[test]
    fn coordinate_round_trip_and_query_scope() {
        for p in [(39.9, 116.4), (22.5, 113.9), (31.2, 121.5)] {
            let bd = crate::track::geom::wgs84_to_bd09(p.0, p.1);
            let w = to_wgs(bd.0, bd.1);
            assert!((w.0 - p.0).abs() < 1e-8 && (w.1 - p.1).abs() < 1e-8);
        }
        let q = query(&fixture().0).unwrap();
        assert!(q.contains("39.8980000,116.3980000,39.9040000,116.4040000"));
        assert!(query(&[]).is_err());
        assert!(validate_fences(&[vec![(0., 0.); 3]]).is_err());
    }
    #[test]
    fn cache_is_school_and_fence_scoped_expires_and_propagates_failure() {
        let dir = std::env::temp_dir().join(format!("school-roads-{}", uuid::Uuid::new_v4()));
        let (f, xml) = fixture();
        let get =
            |school: &str, fences: &[Vec<(f64, f64)>], refresh, now, bytes: Option<Vec<u8>>| {
                cached_network(
                    &dir,
                    school,
                    fences,
                    refresh,
                    now,
                    |_| bytes.ok_or("offline".into()),
                    &mut |_| {},
                )
            };
        assert!(get("a", &f, false, 1000, Some(xml.clone())).is_ok());
        assert!(get("a", &f, false, 1001, None).is_ok());
        assert!(get("b", &f, false, 1001, None).is_err());
        let mut changed = f.clone();
        changed[0][0].0 -= 0.00001;
        assert!(get("a", &changed, false, 1001, None).is_err());
        assert!(get("a", &f, false, 1000 + TTL_MS, None).is_err());
        assert!(get("a", &f, false, 999, None).is_err());
        assert!(get("a", &f, true, 1001, None).is_err());
        assert!(get("a", &f, true, 1002, Some(xml)).is_ok());
        assert!(get("a", &f, false, 1002 + TTL_MS - 1, None).is_ok());
        assert!(parse(b"<osm></osm>").is_err());
        assert!(parse(b"<osm><remark>timeout</remark></osm>").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_boundary_download_is_point_scoped_without_inventing_fences() {
        let dir = std::env::temp_dir().join(format!("point-roads-{}", uuid::Uuid::new_v4()));
        let (_, xml) = fixture();
        let points = [(39.9, 116.4), (39.901, 116.401)]
            .map(|(a, b)| crate::track::geom::wgs84_to_bd09(a, b));
        let net = cached_route_network(
            &dir,
            "a",
            &[],
            &points,
            false,
            1000,
            |q| {
                assert!(q.contains("39.8980000,116.3980000,39.9030000,116.4030000"));
                Ok(xml)
            },
            &mut |_| {},
        )
        .unwrap();
        assert!(net.graph.edge_count() > 0);
        assert!(net.boundaries.is_empty());
        let key = route_scope_key("a", &[], &points);
        let cache: Cache =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{key}.json"))).unwrap())
                .unwrap();
        assert!(cache.fences.is_empty());
        assert_eq!(cache.points.len(), 2);
        let cached = |school, pts: &[(f64, f64)], now| {
            cached_route_network(
                &dir,
                school,
                &[],
                pts,
                false,
                now,
                |_| Err("offline".into()),
                &mut |_| {},
            )
        };
        assert!(cached("a", &points, 1001).is_ok());
        assert!(cached("b", &points, 1001).is_err());
        assert!(cached("a", &points[..1], 1001).is_err());
        assert!(cached("a", &points, 1000 + TTL_MS).is_err());
        assert!(query_coordinates(&[], 0.002).is_err());
        assert!(query_coordinates(&[(f64::NAN, 116.4)], 0.002).is_err());
        assert!(query_coordinates(&[(39., 116.), (40., 116.)], 0.002).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn routable_output_matches_aligning_the_raw_graph_once() {
        fn summary(g: &RoadGraph) -> (usize, Vec<(u64, u64)>, usize) {
            let coords = g
                .graph
                .node_weights()
                .map(|n| (n.coord.lat.to_bits(), n.coord.lon.to_bits()))
                .collect();
            (g.graph.edge_count(), coords, g.boundaries.len())
        }
        let dir = std::env::temp_dir().join(format!("routable-roads-{}", uuid::Uuid::new_v4()));
        let (fences, xml) = fixture();
        let points = [(39.9, 116.4), (39.901, 116.401)]
            .map(|(a, b)| crate::track::geom::wgs84_to_bd09(a, b));
        for (fences, points) in [(fences.clone(), &[][..]), (Vec::new(), &points[..])] {
            let load = |refresh, output, bytes: Option<Vec<u8>>| {
                cached_route_network_as(
                    &dir,
                    "a",
                    &fences,
                    points,
                    refresh,
                    1000,
                    |_| bytes.ok_or("offline".into()),
                    &mut |_| {},
                    output,
                )
            };
            let raw = load(true, Output::Raw, Some(xml.clone())).unwrap();
            let mut want = raw.clone();
            crate::track::generate_road::align_network(&mut want);
            let want = crate::track::generate_road::apply_fences(&want, &fences);
            assert!(want.graph.edge_count() > 0);
            let downloaded = load(true, Output::Routable, Some(xml.clone())).unwrap();
            let cached = load(false, Output::Routable, None).unwrap();
            assert_eq!(summary(&downloaded), summary(&want));
            assert_eq!(summary(&cached), summary(&want));
            assert_eq!(
                summary(&load(false, Output::Raw, None).unwrap()),
                summary(&raw)
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn manual_map_loading_needs_neither_boundary_nor_school_id() {
        let path = std::env::temp_dir().join(format!("manual-road-{}.osm", uuid::Uuid::new_v4()));
        std::fs::write(&path, fixture().1).unwrap();
        let net = load_for_route_cancellable(
            path.to_str().unwrap(),
            "",
            &[],
            &[],
            false,
            &crate::task_control::Control::default(),
            &mut |_| {},
        )
        .unwrap();
        assert!(net.graph.edge_count() > 0);
        assert!(net.boundaries.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saved_maps_can_be_listed_loaded_offline_and_deleted_safely() {
        let root = std::env::temp_dir().join(format!("school-manager-{}", uuid::Uuid::new_v4()));
        crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(root.clone()));
        let (fences, xml) = fixture();
        cached_network(
            &root.join("school_roads"),
            "test-school",
            &fences,
            false,
            1000,
            |_| Ok(xml),
            &mut |_| {},
        )
        .unwrap();
        save_context("test-school", "接口校区名", &fences, &[(39.9, 116.4)]);
        let list = entries();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "接口校区名");
        assert!(list[0].bytes > 0);
        let (_, entry) = read_cache(&list[0].key).unwrap();
        assert_eq!(scope_key("", &entry.fences), scope_key("", &fences));
        assert_eq!(entry.points.len(), 1);
        assert!(clear_cache("../other").is_err());
        clear_cache(&list[0].key).unwrap();
        assert!(entries().is_empty());
        crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = None);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|f| f.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    fn graph_summary(g: &RoadGraph) -> Vec<u8> {
        postcard::to_stdvec(g).unwrap()
    }

    /// Replace the record with same-length garbage and restore its mtime: the
    /// sidecars still describe it, but its XML can no longer be parsed.
    fn garble_keeping_stamp(path: &Path) {
        let m = std::fs::metadata(path).unwrap();
        std::fs::write(path, vec![b'#'; m.len() as usize]).unwrap();
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(m.modified().unwrap()).unwrap();
        assert_eq!(stamp(&std::fs::metadata(path).unwrap()), stamp(&m));
    }

    struct TestDataDir(std::path::PathBuf);
    impl TestDataDir {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("{tag}-{}", uuid::Uuid::new_v4()));
            crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(root.clone()));
            TestDataDir(root)
        }
    }
    impl Drop for TestDataDir {
        fn drop(&mut self) {
            crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn binary_graph_matches_xml_parse_and_serves_cache_without_xml() {
        let _root = TestDataDir::new("graph-cache");
        let dir = cache_dir();
        let (fences, xml) = fixture();
        let parsed = parse(&xml).unwrap();
        let raw =
            cached_network(&dir, "s", &fences, false, 1000, |_| Ok(xml), &mut |_| {}).unwrap();
        let key = scope_key("s", &fences);
        assert_eq!(
            files(&dir),
            [
                format!("{key}.graph"),
                format!("{key}.json"),
                format!("{key}.meta.json")
            ]
        );
        // The binary decodes to exactly the parsed graph, header bound to the XML.
        let meta = read_meta(&dir, &key).unwrap();
        assert_eq!(
            graph_summary(&read_graph(&dir, &meta).unwrap()),
            graph_summary(&parsed)
        );
        assert_eq!(graph_summary(&raw), graph_summary(&parsed));

        // With the XML unusable, list/load/route all run from the sidecars alone.
        garble_keeping_stamp(&dir.join(format!("{key}.json")));
        let list = entries();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].school, "s");
        assert_eq!(list[0].loaded_ms, 1000);
        assert!(list[0].bytes > 0);
        let offline = |output| {
            cached_route_network_as(
                &dir,
                "s",
                &fences,
                &[],
                false,
                1001,
                |_| Err("offline".into()),
                &mut |_| {},
                output,
            )
        };
        assert_eq!(
            graph_summary(&offline(Output::Raw).unwrap()),
            graph_summary(&parsed)
        );
        assert_eq!(
            graph_summary(&offline(Output::Routable).unwrap()),
            graph_summary(&routable(parsed.clone(), &fences))
        );
        assert_eq!(
            graph_summary(&read_cache(&key).unwrap().0),
            graph_summary(&parsed)
        );
        // Still subject to the validity window.
        assert!(cached_network(
            &dir,
            "s",
            &fences,
            false,
            1000 + TTL_MS,
            |_| Err("offline".into()),
            &mut |_| {}
        )
        .is_err());
        // A rewritten record (different stamp) invalidates the sidecars.
        std::fs::write(dir.join(format!("{key}.json")), b"{}").unwrap();
        assert!(entries().is_empty());
        assert!(offline(Output::Raw).is_err());
    }

    #[test]
    fn old_cache_is_read_and_upgraded_and_clear_removes_everything() {
        let _root = TestDataDir::new("old-cache");
        let dir = cache_dir();
        let (fences, xml) = fixture();
        let key = scope_key("s", &fences);
        // Exactly what earlier builds wrote: the JSON record alone.
        std::fs::create_dir_all(&dir).unwrap();
        let old = Cache {
            key: key.clone(),
            loaded_ms: 1000,
            xml: String::from_utf8(xml.clone()).unwrap(),
            school: "s".into(),
            name: "旧缓存".into(),
            fences: fences.clone(),
            points: vec![(39.9, 116.4)],
        };
        std::fs::write(
            dir.join(format!("{key}.json")),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        // Listing reads the record once and writes only the metadata sidecar.
        let list = entries();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].points.len()), ("旧缓存", 1));
        assert_eq!(
            files(&dir),
            [format!("{key}.json"), format!("{key}.meta.json")]
        );
        // Loading parses the XML once and adds the binary graph.
        let net = cached_network(
            &dir,
            "s",
            &fences,
            false,
            1001,
            |_| Err("offline".into()),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(graph_summary(&net), graph_summary(&parse(&xml).unwrap()));
        assert_eq!(files(&dir).len(), 3);
        // From now on the XML is not needed.
        garble_keeping_stamp(&dir.join(format!("{key}.json")));
        assert!(cached_network(
            &dir,
            "s",
            &fences,
            false,
            1002,
            |_| Err("offline".into()),
            &mut |_| {}
        )
        .is_ok());
        assert_eq!(entries()[0].name, "旧缓存");
        // Other schools / fences stay isolated even with sidecars present.
        assert!(cached_network(
            &dir,
            "t",
            &fences,
            false,
            1002,
            |_| Err("offline".into()),
            &mut |_| {}
        )
        .is_err());

        let stray = dir.join("unrelated.txt");
        std::fs::write(&stray, b"keep").unwrap();
        clear_cache(&key).unwrap();
        assert_eq!(files(&dir), ["unrelated.txt"]);
        assert!(entries().is_empty());
        assert!(clear_cache(&key).is_err());
    }

    #[test]
    fn corrupt_or_mismatched_binary_falls_back_to_xml_and_is_rebuilt() {
        let dir = std::env::temp_dir().join(format!("graph-fallback-{}", uuid::Uuid::new_v4()));
        let (fences, xml) = fixture();
        let parsed = graph_summary(&parse(&xml).unwrap());
        let load = |now| {
            cached_network(
                &dir,
                "s",
                &fences,
                false,
                now,
                |_| Err("offline".into()),
                &mut |_| {},
            )
        };
        cached_network(
            &dir,
            "s",
            &fences,
            false,
            1000,
            |_| Ok(xml.clone()),
            &mut |_| {},
        )
        .unwrap();
        let key = scope_key("s", &fences);
        let graph = dir.join(format!("{key}.graph"));
        let good = std::fs::read(&graph).unwrap();
        let net = parse(&xml).unwrap();
        let bad_headers = [
            GraphHeader {
                xml_sha256: "0".repeat(64),
                ..GraphHeader::new(&key, "")
            },
            GraphHeader {
                graph_format: route_planner::load::GRAPH_FORMAT + 1,
                ..GraphHeader::new(&key, &read_meta(&dir, &key).unwrap().xml_sha256)
            },
            GraphHeader {
                magic: *b"NOTROADS",
                ..GraphHeader::new(&key, &read_meta(&dir, &key).unwrap().xml_sha256)
            },
        ];
        let mut corrupt = vec![
            Vec::new(),
            good[..good.len() / 2].to_vec(),
            good.iter().map(|b| b ^ 0x5a).collect(),
        ];
        corrupt.extend(bad_headers.iter().map(|h| encode_graph(h, &net).unwrap()));
        for bytes in corrupt {
            std::fs::write(&graph, &bytes).unwrap();
            assert_eq!(graph_summary(&load(1001).unwrap()), parsed);
            assert_eq!(std::fs::read(&graph).unwrap(), good, "binary rebuilt");
        }
        // A missing or corrupt metadata sidecar is rebuilt the same way.
        std::fs::write(dir.join(format!("{key}.meta.json")), b"garbage").unwrap();
        std::fs::remove_file(&graph).unwrap();
        assert_eq!(graph_summary(&load(1001).unwrap()), parsed);
        assert!(read_meta(&dir, &key).is_some());
        assert_eq!(std::fs::read(&graph).unwrap(), good);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_context_keeps_binary_valid_and_skips_unchanged_labels() {
        let _root = TestDataDir::new("context-cache");
        let dir = cache_dir();
        let (fences, xml) = fixture();
        cached_network(&dir, "s", &fences, false, 1000, |_| Ok(xml), &mut |_| {}).unwrap();
        let key = scope_key("s", &fences);
        let record = dir.join(format!("{key}.json"));
        save_context("s", "校区", &fences, &[(39.9, 116.4)]);
        let after = std::fs::read(&record).unwrap();
        let (cache, _) = read_record(&dir, &key).unwrap();
        assert_eq!(cache.name, "校区");
        let meta = read_meta(&dir, &key).unwrap();
        assert_eq!(meta.name, "校区");
        assert!(
            read_graph(&dir, &meta).is_some(),
            "XML unchanged, binary still valid"
        );
        // Same label again: the record is not rewritten.
        let stamp_before = stamp(&std::fs::metadata(&record).unwrap());
        save_context("s", "校区", &fences, &[(39.9, 116.4)]);
        assert_eq!(stamp(&std::fs::metadata(&record).unwrap()), stamp_before);
        assert_eq!(std::fs::read(&record).unwrap(), after);
    }

    /// Release timing of a cache hit, old path (record JSON + XML parse) vs
    /// sidecar binary. `ROAD_GRID_SIDE` sets the synthetic grid side (default 142):
    /// `cargo test --release --lib cache_hit_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn cache_hit_timing() {
        let n: usize = std::env::var("ROAD_GRID_SIDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(142);
        let mut xml = String::from("<osm version=\"0.6\">\n");
        let id = |i: usize, j: usize| 1 + j * n + i;
        for j in 0..n {
            for i in 0..n {
                let (lat, lon) = (39.9 + j as f64 * 0.0001, 116.4 + i as f64 * 0.0001);
                xml += &format!(
                    "  <node id=\"{}\" lat=\"{lat:.7}\" lon=\"{lon:.7}\"/>\n",
                    id(i, j)
                );
            }
        }
        for (w, line) in (0..n).flat_map(|k| [(k, true), (k, false)]).enumerate() {
            xml += &format!("  <way id=\"{}\">\n", w + 1);
            for m in 0..n {
                let (i, j) = if line.1 { (m, line.0) } else { (line.0, m) };
                xml += &format!("    <nd ref=\"{}\"/>\n", id(i, j));
            }
            xml += "    <tag k=\"highway\" v=\"residential\"/>\n  </way>\n";
        }
        xml += "</osm>\n";
        let dir = std::env::temp_dir().join(format!("road-timing-{}", uuid::Uuid::new_v4()));
        let points = [(39.9, 116.4)].map(|(a, b)| crate::track::geom::wgs84_to_bd09(a, b));
        let bytes = xml.into_bytes();
        let xml_len = bytes.len();
        cached_route_network(
            &dir,
            "s",
            &[],
            &points,
            false,
            1000,
            |_| Ok(bytes),
            &mut |_| {},
        )
        .unwrap();
        let key = route_scope_key("s", &[], &points);
        let t = std::time::Instant::now();
        let (cache, _) = read_record(&dir, &key).unwrap();
        // Before sidecars, listing paid this per entry.
        let old_list = t.elapsed();
        let old = parse(cache.xml.as_bytes()).unwrap();
        let old_path = t.elapsed();
        let t = std::time::Instant::now();
        let meta = read_meta(&dir, &key).unwrap();
        let new = read_graph(&dir, &meta).unwrap();
        let new_path = t.elapsed();
        assert_eq!(graph_summary(&old), graph_summary(&new));
        let t = std::time::Instant::now();
        let list = entries_in(&dir);
        let list_time = t.elapsed();
        assert_eq!(list.len(), 1);
        let size = |ext: &str| {
            std::fs::metadata(dir.join(format!("{key}{ext}")))
                .unwrap()
                .len()
                / 1024
        };
        println!(
            "nodes={} edges={} xml={} KiB record={} KiB graph={} KiB | record+parse={old_path:?} meta+binary={new_path:?} | list: record={old_list:?} meta={list_time:?}",
            new.graph.node_count(),
            new.graph.edge_count(),
            xml_len / 1024,
            size(".json"),
            size(GRAPH_SUFFIX),
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
