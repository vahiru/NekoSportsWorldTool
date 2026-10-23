//! Route-scoped elevation acquisition. A fallback replaces the entire grid,
//! never individual samples across different vertical datums.

use super::{convert_coordinate, CoordinateSystem, TerrainGrid};
use crate::task_control::Control;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_GRID_POINTS: usize = 1024;
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const BATCH_SIZE: usize = 100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElevationProvider {
    #[default]
    Local,
    Auto,
    OpenMeteo,
    #[serde(rename = "opentopodata", alias = "open_topo_data")]
    OpenTopoData,
    Mapbox,
}

impl ElevationProvider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Auto => "auto",
            Self::OpenMeteo => "open_meteo",
            Self::OpenTopoData => "opentopodata",
            Self::Mapbox => "mapbox",
        }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local" => Ok(Self::Local),
            "auto" => Ok(Self::Auto),
            "open_meteo" | "open-meteo" => Ok(Self::OpenMeteo),
            "open_topo_data" | "opentopodata" => Ok(Self::OpenTopoData),
            "mapbox" => Ok(Self::Mapbox),
            _ => Err("高程源应为 local|auto|open_meteo|opentopodata|mapbox".into()),
        }
    }
    fn metadata(self) -> (&'static str, &'static str, f64) {
        match self {
            Self::OpenMeteo => (
                "Open-Meteo / Copernicus DEM 2021 GLO-90 (Copernicus attribution required)",
                "EGM2008",
                90.0,
            ),
            Self::OpenTopoData => ("OpenTopoData / SRTM90m", "EGM96", 90.0),
            Self::Mapbox => (
                "Mapbox Terrain-RGB (Mapbox attribution required; source resolutions vary)",
                "mixed:Mapbox source datums, unconverted",
                30.0,
            ),
            _ => ("", "", 90.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ElevationOptions {
    pub provider: ElevationProvider,
    /// Output grid spacing, not a claim about DEM accuracy.
    pub grid_spacing_m: u32,
    pub cache_days: u32,
    pub timeout_s: u32,
    pub mapbox_zoom: u8,
}

impl Default for ElevationOptions {
    fn default() -> Self {
        Self {
            provider: ElevationProvider::Local,
            grid_spacing_m: 90,
            cache_days: 30,
            timeout_s: 15,
            mapbox_zoom: 14,
        }
    }
}
impl ElevationOptions {
    pub fn validate(&self) -> Result<(), String> {
        if !(10..=1000).contains(&self.grid_spacing_m)
            || !(1..=60).contains(&self.timeout_s)
            || !(1..=15).contains(&self.mapbox_zoom)
            || self.cache_days > 3650
        {
            return Err(
                "高程参数无效：间距 10..1000m，超时 1..60s，Mapbox zoom 1..15，缓存 0..3650 天"
                    .into(),
            );
        }
        Ok(())
    }
}

pub enum TerrainSource {
    Local(TerrainGrid),
    Online(ElevationOptions),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GridSpec {
    west: f64,
    south: f64,
    step: f64,
    rows: usize,
    cols: usize,
}

impl GridSpec {
    fn around(
        points: &[(f64, f64)],
        system: CoordinateSystem,
        spacing: u32,
    ) -> Result<Self, String> {
        if points.is_empty() {
            return Err("自动高程需要路线坐标".into());
        }
        let (mut west, mut south, mut east, mut north) =
            (180.0_f64, 90.0_f64, -180.0_f64, -90.0_f64);
        for &(lat, lon) in points {
            let (lat, lon) = convert_coordinate(lat, lon, system, CoordinateSystem::Wgs84);
            if !lat.is_finite() || !lon.is_finite() || lat.abs() > 85.0 || lon.abs() > 180.0 {
                return Err("自动高程路线经纬度无效（纬度须在 ±85° 内）".into());
            }
            west = west.min(lon);
            east = east.max(lon);
            south = south.min(lat);
            north = north.max(lat);
        }
        if east - west > 180.0 {
            return Err("自动高程暂不支持跨日期变更线".into());
        }
        let step = spacing as f64 / route_planner::geo::MET_PER_DEG_LAT;
        west = ((west / step).floor() - 1.0) * step;
        south = ((south / step).floor() - 1.0) * step;
        east = ((east / step).ceil() + 1.0) * step;
        north = ((north / step).ceil() + 1.0) * step;
        if west < -180.0 || east > 180.0 || south < -85.051 || north > 85.051 {
            return Err("自动高程网格缓冲区超出支持范围".into());
        }
        let cols = ((east - west) / step).round() as usize + 1;
        let rows = ((north - south) / step).round() as usize + 1;
        if cols.checked_mul(rows).is_none_or(|n| n > MAX_GRID_POINTS) {
            return Err("自动高程网格超过 1024 点，请增大 grid_spacing_m 或缩小路线范围".into());
        }
        Ok(Self {
            west,
            south,
            step,
            rows,
            cols,
        })
    }
    fn locations(&self) -> Vec<(f64, f64)> {
        (0..self.rows)
            .flat_map(|j| {
                (0..self.cols).map(move |i| {
                    (
                        self.south + j as f64 * self.step,
                        self.west + i as f64 * self.step,
                    )
                })
            })
            .collect()
    }
    fn matches(&self, grid: &TerrainGrid) -> bool {
        grid.coordinate_system == CoordinateSystem::Wgs84
            // JSON float parsing may differ by an ulp; tolerate far less than
            // a millimetre, not a different grid cell or spatial footprint.
            && (grid.west-self.west).abs()<1e-10
            && (grid.south-self.south).abs()<1e-10
            && (grid.longitude_step-self.step).abs()<1e-12
            && (grid.latitude_step-self.step).abs()<1e-12
            && grid.heights_m.len() == self.rows
            && grid
                .heights_m
                .iter()
                .all(|r| r.len() == self.cols && r.iter().all(Option::is_some))
    }
}

#[derive(Serialize, Deserialize)]
struct CachedGrid {
    version: u32,
    provider: ElevationProvider,
    fetched_at_s: u64,
    grid: TerrainGrid,
}

/// No endpoint override in user configuration: credentials are only sent to the
/// built-in Mapbox host. Tests substitute transports, not production hostnames.
pub fn fetch_grid(
    points: &[(f64, f64)],
    system: CoordinateSystem,
    options: &ElevationOptions,
    control: &Control,
    log: &mut dyn FnMut(&str),
) -> Result<TerrainGrid, String> {
    let transport = HttpTransport::new(options);
    fetch_with(
        points,
        system,
        options,
        &crate::platform::data_dir().join("terrain_cache"),
        control,
        log,
        &transport,
    )
}

#[allow(clippy::too_many_arguments)]
fn fetch_with(
    points: &[(f64, f64)],
    system: CoordinateSystem,
    options: &ElevationOptions,
    cache_dir: &Path,
    control: &Control,
    log: &mut dyn FnMut(&str),
    transport: &dyn Transport,
) -> Result<TerrainGrid, String> {
    options.validate()?;
    control.check()?;
    let spec = GridSpec::around(points, system, options.grid_spacing_m)?;
    let providers: &[ElevationProvider] = match options.provider {
        ElevationProvider::Auto => &[
            ElevationProvider::OpenMeteo,
            ElevationProvider::OpenTopoData,
            ElevationProvider::Mapbox,
        ],
        ElevationProvider::Local => return Err("local 高程源需要本地文件，不能自动下载".into()),
        _ => std::slice::from_ref(&options.provider),
    };
    let mut candidates = Vec::new();
    for &provider in providers {
        control.check()?;
        let fingerprint = serde_json::to_vec(&(1, provider, &spec, options.mapbox_zoom))
            .map_err(|e| e.to_string())?;
        let key = format!("{:x}.json", Sha256::digest(&fingerprint));
        let path = cache_dir.join(key);
        candidates.push((provider, path));
    }
    // A fresh fallback cache should work offline without first timing out on
    // higher-priority providers that have no cached grid.
    for (provider, path) in &candidates {
        control.check()?;
        if let Some(grid) = read_cache(path, *provider, &spec, options) {
            log(&format!("[terrain] {} 命中本地高程缓存", provider.name()));
            return Ok(grid);
        }
    }
    let mut errors = Vec::new();
    for (provider, path) in candidates {
        control.check()?;
        log(&format!(
            "[terrain] {} 拉取 {}×{} 高程网格…",
            provider.name(),
            spec.cols,
            spec.rows
        ));
        match download(provider, &spec, options, control, transport) {
            Ok(grid) => {
                control.check()?;
                if write_cache(&path, provider, &grid).is_err() {
                    log("[terrain] 高程已获取，但缓存写入失败；本次继续使用下载数据");
                }
                return Ok(grid);
            }
            Err(error) => {
                control.check()?;
                log(&format!("[terrain] {} 获取失败：{error}", provider.name()));
                errors.push(format!("{}: {error}", provider.name()));
            }
        }
    }
    Err(format!("自动高程源全部失败：{}", errors.join("；")))
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
fn resolution(provider: ElevationProvider, spec: &GridSpec, options: &ElevationOptions) -> f64 {
    let native = provider.metadata().2;
    let pixel = if provider == ElevationProvider::Mapbox {
        let north = spec.south + (spec.rows - 1) as f64 * spec.step;
        let latitude = if spec.south <= 0.0 && north >= 0.0 {
            0.0
        } else {
            spec.south.abs().min(north.abs())
        };
        40_075_016.685_578_49 / (256.0 * (1u32 << options.mapbox_zoom) as f64)
            * latitude.to_radians().cos()
    } else {
        0.0
    };
    (options.grid_spacing_m as f64).max(native).max(pixel)
}
fn read_cache(
    path: &Path,
    provider: ElevationProvider,
    spec: &GridSpec,
    options: &ElevationOptions,
) -> Option<TerrainGrid> {
    if options.cache_days == 0 || std::fs::metadata(path).ok()?.len() > MAX_RESPONSE_BYTES {
        return None;
    }
    let cached: CachedGrid = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let now = now_s();
    let (source, datum, _) = provider.metadata();
    if cached.version != 1
        || cached.provider != provider
        || cached.fetched_at_s > now
        || now - cached.fetched_at_s > options.cache_days as u64 * 86400
        || !spec.matches(&cached.grid)
        || cached.grid.validate().is_err()
        || cached.grid.source != source
        || cached.grid.vertical_datum != datum
        || (cached.grid.resolution_m - resolution(provider, spec, options)).abs() > 1e-8
    {
        return None;
    }
    Some(cached.grid)
}
fn write_cache(path: &Path, provider: ElevationProvider, grid: &TerrainGrid) -> Result<(), String> {
    let parent = path.parent().ok_or("无效缓存目录")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(
            file,
            &CachedGrid {
                version: 1,
                provider,
                fetched_at_s: now_s(),
                grid: grid.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

trait Transport {
    fn batch(&self, provider: ElevationProvider, points: &[(f64, f64)]) -> Result<Value, String>;
    fn tile(&self, zoom: u8, x: u32, y: u32) -> Result<Vec<u8>, String>;
    fn wait(&self, provider: ElevationProvider, control: &Control) -> Result<(), String>;
}

struct HttpTransport {
    agent: ureq::Agent,
}
impl HttpTransport {
    fn new(options: &ElevationOptions) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(options.timeout_s as u64))
                .redirects(0)
                .user_agent(concat!("NekoSportsWorldTool/", env!("CARGO_PKG_VERSION")))
                .build(),
        }
    }
    fn bytes(&self, request: ureq::Request) -> Result<Vec<u8>, String> {
        // Never format ureq errors: they can contain Mapbox's query token.
        let response = request.call().map_err(|e| match e {
            ureq::Error::Status(status, _) => format!("HTTP {status}"),
            ureq::Error::Transport(_) => "网络连接或超时错误".into(),
        })?;
        if !(200..300).contains(&response.status()) {
            return Err(format!("HTTP {}", response.status()));
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "读取高程响应失败")?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("高程响应超过 2MiB".into());
        }
        Ok(bytes)
    }
}
impl Transport for HttpTransport {
    fn batch(&self, provider: ElevationProvider, points: &[(f64, f64)]) -> Result<Value, String> {
        let request = match provider {
            ElevationProvider::OpenMeteo => self
                .agent
                .get("https://api.open-meteo.com/v1/elevation")
                .query(
                    "latitude",
                    &points
                        .iter()
                        .map(|p| format!("{:.8}", p.0))
                        .collect::<Vec<_>>()
                        .join(","),
                )
                .query(
                    "longitude",
                    &points
                        .iter()
                        .map(|p| format!("{:.8}", p.1))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            ElevationProvider::OpenTopoData => self
                .agent
                .get("https://api.opentopodata.org/v1/srtm90m")
                .query(
                    "locations",
                    &points
                        .iter()
                        .map(|p| format!("{:.8},{:.8}", p.0, p.1))
                        .collect::<Vec<_>>()
                        .join("|"),
                )
                .query("interpolation", "bilinear"),
            _ => return Err("该高程源不支持点查询".into()),
        };
        serde_json::from_slice(&self.bytes(request)?).map_err(|_| "高程服务返回无效 JSON".into())
    }
    fn tile(&self, zoom: u8, x: u32, y: u32) -> Result<Vec<u8>, String> {
        let token = std::env::var("MAPBOX_ACCESS_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or("Mapbox 需要环境变量 MAPBOX_ACCESS_TOKEN")?;
        let request = self
            .agent
            .get(&format!(
                "https://api.mapbox.com/v4/mapbox.terrain-rgb/{zoom}/{x}/{y}.pngraw"
            ))
            .query("access_token", token.trim());
        self.bytes(request)
    }
    fn wait(&self, provider: ElevationProvider, control: &Control) -> Result<(), String> {
        // Public OpenTopoData is limited to 1 request/s. Other public queries
        // are also spaced out to avoid bursts (not a replacement for daily quotas).
        let delay = if provider == ElevationProvider::OpenTopoData {
            1100
        } else {
            250
        };
        static LAST_REQUEST: std::sync::Mutex<[Option<Instant>; 3]> =
            std::sync::Mutex::new([None; 3]);
        let index = match provider {
            ElevationProvider::OpenMeteo => 0,
            ElevationProvider::OpenTopoData => 1,
            _ => 2,
        };
        let mut times = LAST_REQUEST.lock().map_err(|_| "高程请求限速器不可用")?;
        let until = times[index].map(|time| time + Duration::from_millis(delay));
        while until.is_some_and(|deadline| Instant::now() < deadline) {
            control.check()?;
            std::thread::sleep(Duration::from_millis(50));
        }
        times[index] = Some(Instant::now());
        control.check()
    }
}

fn download(
    provider: ElevationProvider,
    spec: &GridSpec,
    options: &ElevationOptions,
    control: &Control,
    transport: &dyn Transport,
) -> Result<TerrainGrid, String> {
    let points = spec.locations();
    let values = if provider == ElevationProvider::Mapbox {
        mapbox_values(&points, options.mapbox_zoom, control, transport)?
    } else {
        let mut values = Vec::with_capacity(points.len());
        for chunk in points.chunks(BATCH_SIZE) {
            control.check()?;
            transport.wait(provider, control)?;
            let response = transport.batch(provider, chunk)?;
            values.extend(parse_batch(provider, chunk, &response)?);
        }
        values
    };
    let (source, datum, _) = provider.metadata();
    let grid = TerrainGrid {
        source: source.into(),
        vertical_datum: datum.into(),
        coordinate_system: CoordinateSystem::Wgs84,
        resolution_m: resolution(provider, spec, options),
        west: spec.west,
        south: spec.south,
        longitude_step: spec.step,
        latitude_step: spec.step,
        heights_m: values
            .chunks(spec.cols)
            .map(|row| row.iter().copied().map(Some).collect())
            .collect(),
    };
    grid.validate()?;
    if !spec.matches(&grid) {
        return Err("高程网格尺寸或覆盖范围不符".into());
    }
    Ok(grid)
}

fn parse_batch(
    provider: ElevationProvider,
    points: &[(f64, f64)],
    response: &Value,
) -> Result<Vec<f64>, String> {
    let values = match provider {
        ElevationProvider::OpenMeteo => response
            .get("elevation")
            .and_then(Value::as_array)
            .ok_or("Open-Meteo 响应缺少 elevation 数组")?,
        ElevationProvider::OpenTopoData => {
            if response["status"] != "OK" {
                return Err("OpenTopoData 未返回 OK（限流、数据集或请求错误）".into());
            }
            response
                .get("results")
                .and_then(Value::as_array)
                .ok_or("OpenTopoData 响应缺少 results")?
        }
        _ => return Err("未知点查询高程源".into()),
    };
    if values.len() != points.len() {
        return Err("高程响应点数与请求不符".into());
    }
    values
        .iter()
        .zip(points)
        .enumerate()
        .map(|(i, (value, point))| {
            let height = if provider == ElevationProvider::OpenTopoData {
                let location = &value["location"];
                if location["lat"]
                    .as_f64()
                    .is_none_or(|v| (v - point.0).abs() > 1e-6)
                    || location["lng"]
                        .as_f64()
                        .is_none_or(|v| (v - point.1).abs() > 1e-6)
                {
                    return Err(format!("高程响应第 {i} 点坐标或顺序不匹配"));
                }
                value["elevation"].as_f64()
            } else {
                value.as_f64()
            };
            height
                .filter(|h| h.is_finite() && (-500.0..=9000.0).contains(h))
                .ok_or_else(|| format!("高程响应第 {i} 点缺测或高度无效"))
        })
        .collect()
}

fn tile_pixel(lat: f64, lon: f64, zoom: u8) -> (u32, u32, u32, u32) {
    let size = 256.0 * (1u32 << zoom) as f64;
    let x = ((lon + 180.0) / 360.0 * size)
        .clamp(0.0, size - 1.0)
        .floor() as u32;
    let y = ((1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * size)
        .clamp(0.0, size - 1.0)
        .floor() as u32;
    (x / 256, y / 256, x % 256, y % 256)
}
fn mapbox_values(
    points: &[(f64, f64)],
    zoom: u8,
    control: &Control,
    transport: &dyn Transport,
) -> Result<Vec<f64>, String> {
    let mut tiles = HashMap::new();
    let mut heights = Vec::with_capacity(points.len());
    for &(lat, lon) in points {
        control.check()?;
        let (x, y, px, py) = tile_pixel(lat, lon, zoom);
        if !tiles.contains_key(&(x, y)) {
            if tiles.len() >= 64 {
                return Err("Mapbox 请求超过 64 个瓦片，请降低 mapbox_zoom 或缩小范围".into());
            }
            transport.wait(ElevationProvider::Mapbox, control)?;
            let bytes = transport.tile(zoom, x, y)?;
            let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&bytes))
                .map_err(|_| "Mapbox 返回无效 PNG（可能无地形覆盖）")?;
            use image::ImageDecoder;
            if decoder.dimensions() != (256, 256) {
                return Err("Mapbox 高程瓦片必须为 256×256 PNG".into());
            }
            if !matches!(
                decoder.color_type(),
                image::ColorType::Rgb8 | image::ColorType::Rgba8
            ) {
                return Err("Mapbox 高程瓦片必须为 8 位 RGB/RGBA 数据".into());
            }
            let tile = image::DynamicImage::from_decoder(decoder)
                .map_err(|_| "Mapbox PNG 解码失败")?
                .to_rgba8();
            tiles.insert((x, y), tile);
        }
        let pixel = tiles[&(x, y)].get_pixel(px, py).0;
        if pixel[3] == 0 {
            return Err("Mapbox 高程瓦片含透明缺测像素".into());
        }
        let height = -10000.0
            + (pixel[0] as u32 * 65536 + pixel[1] as u32 * 256 + pixel[2] as u32) as f64 * 0.1;
        if !(-500.0..=9000.0).contains(&height) {
            return Err("Mapbox RGB 高程无效或缺测".into());
        }
        heights.push(height);
    }
    Ok(heights)
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
