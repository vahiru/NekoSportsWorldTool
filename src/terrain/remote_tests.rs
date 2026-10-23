use super::*;
use serde_json::json;
use std::cell::RefCell;

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("neko-elevation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Mock<'a> {
    calls: RefCell<Vec<(ElevationProvider, usize)>>,
    fail_meteo_after_first: bool,
    cancel_after_batch: Option<&'a Control>,
}
impl Transport for Mock<'_> {
    fn batch(&self, provider: ElevationProvider, points: &[(f64, f64)]) -> Result<Value, String> {
        let previous = self
            .calls
            .borrow()
            .iter()
            .filter(|p| p.0 == provider)
            .count();
        self.calls.borrow_mut().push((provider, points.len()));
        if self.fail_meteo_after_first && provider == ElevationProvider::OpenMeteo && previous > 0 {
            return Err("fixture timeout".into());
        }
        if let Some(control) = self.cancel_after_batch {
            control.cancel();
        }
        match provider {
            ElevationProvider::OpenMeteo => Ok(json!({"elevation":vec![111.0;points.len()]})),
            ElevationProvider::OpenTopoData => Ok(
                json!({"status":"OK","results":points.iter().map(|&(lat,lon)|
                json!({"location":{"lat":lat,"lng":lon},"elevation":222.0})).collect::<Vec<_>>()}),
            ),
            _ => Err("unexpected provider".into()),
        }
    }
    fn tile(&self, _zoom: u8, _x: u32, _y: u32) -> Result<Vec<u8>, String> {
        self.calls.borrow_mut().push((ElevationProvider::Mapbox, 1));
        // Terrain-RGB 0x01adb0 = 110000 → 1000m.
        let tile = image::RgbaImage::from_pixel(256, 256, image::Rgba([1, 173, 176, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        tile.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        Ok(bytes.into_inner())
    }
    fn wait(&self, _provider: ElevationProvider, control: &Control) -> Result<(), String> {
        control.check()
    }
}

fn options(provider: ElevationProvider) -> ElevationOptions {
    ElevationOptions {
        provider,
        ..Default::default()
    }
}
fn points() -> Vec<(f64, f64)> {
    vec![(30.0002, 110.0002), (30.0018, 110.0018)]
}
fn acquire(
    options: &ElevationOptions,
    dir: &Path,
    mock: &dyn Transport,
) -> Result<TerrainGrid, String> {
    fetch_with(
        &points(),
        CoordinateSystem::Wgs84,
        options,
        dir,
        &Control::default(),
        &mut |_| {},
        mock,
    )
}

#[test]
fn batch_order_null_and_error_status_are_checked() {
    let points = points();
    assert!(parse_batch(
        ElevationProvider::OpenMeteo,
        &points,
        &json!({"elevation":[1,null]})
    )
    .is_err());
    assert!(parse_batch(
        ElevationProvider::OpenMeteo,
        &points,
        &json!({"elevation":[1]})
    )
    .is_err());
    assert!(parse_batch(
        ElevationProvider::OpenTopoData,
        &points,
        &json!({"status":"INVALID_REQUEST","results":[]})
    )
    .is_err());
    let mock = Mock::default();
    let mut response = mock
        .batch(ElevationProvider::OpenTopoData, &points)
        .unwrap();
    assert_eq!(
        parse_batch(ElevationProvider::OpenTopoData, &points, &response).unwrap(),
        vec![222.0, 222.0]
    );
    response["results"].as_array_mut().unwrap().reverse();
    assert!(parse_batch(ElevationProvider::OpenTopoData, &points, &response).is_err());
}

#[test]
fn cache_replays_offline_and_corruption_or_expiry_triggers_refresh() {
    let fixture = Fixture::new();
    let mock = Mock::default();
    let config = options(ElevationProvider::OpenMeteo);
    let grid = acquire(&config, &fixture.0, &mock).unwrap();
    assert_eq!(grid.vertical_datum, "EGM2008");
    let count = mock.calls.borrow().len();
    acquire(&config, &fixture.0, &mock).unwrap();
    assert_eq!(mock.calls.borrow().len(), count);
    let path = std::fs::read_dir(&fixture.0)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&path, b"broken cache").unwrap();
    acquire(&config, &fixture.0, &mock).unwrap();
    assert_eq!(mock.calls.borrow().len(), count + 1);
    let mut stale: CachedGrid = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stale.fetched_at_s = 0;
    std::fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
    acquire(&config, &fixture.0, &mock).unwrap();
    assert_eq!(mock.calls.borrow().len(), count + 2);
}

#[test]
fn auto_fallback_discards_partial_grid_and_prefers_fresh_fallback_cache() {
    let fixture = Fixture::new();
    let mock = Mock {
        fail_meteo_after_first: true,
        ..Default::default()
    };
    let options = ElevationOptions {
        provider: ElevationProvider::Auto,
        grid_spacing_m: 10,
        ..Default::default()
    };
    let grid = acquire(&options, &fixture.0, &mock).unwrap();
    assert_eq!(grid.vertical_datum, "EGM96");
    assert!(grid.heights_m.iter().flatten().all(|h| *h == Some(222.0)));
    assert!(mock.calls.borrow().iter().all(|(_, n)| *n <= 100));
    let count = mock.calls.borrow().len();
    let cached = acquire(&options, &fixture.0, &mock).unwrap();
    assert_eq!(cached.source, grid.source);
    assert_eq!(mock.calls.borrow().len(), count);
}

#[test]
fn cancellation_does_not_fall_back_or_cache_partial_data() {
    let fixture = Fixture::new();
    let control = Control::default();
    let mock = Mock {
        cancel_after_batch: Some(&control),
        ..Default::default()
    };
    let error = fetch_with(
        &points(),
        CoordinateSystem::Wgs84,
        &options(ElevationProvider::Auto),
        &fixture.0,
        &control,
        &mut |_| {},
        &mock,
    )
    .unwrap_err();
    assert!(error.contains("取消"));
    assert_eq!(mock.calls.borrow().len(), 1);
    assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[test]
fn coordinate_conversion_padding_and_grid_limit() {
    let p = points();
    let bd: Vec<_> = p
        .iter()
        .map(|&(lat, lon)| {
            convert_coordinate(lat, lon, CoordinateSystem::Wgs84, CoordinateSystem::Bd09)
        })
        .collect();
    let spec = GridSpec::around(&bd, CoordinateSystem::Bd09, 90).unwrap();
    assert!(spec.west < p[0].1 && spec.south < p[0].0);
    assert!(spec.west + (spec.cols - 1) as f64 * spec.step > p[1].1);
    assert!(GridSpec::around(&[(0.0, 0.0), (10.0, 10.0)], CoordinateSystem::Wgs84, 10).is_err());
    assert!(GridSpec::around(&[(90.0, 0.0)], CoordinateSystem::Wgs84, 90).is_err());
}

#[test]
fn mapbox_tile_projection_rgb_decode_and_deduplication() {
    let fixture = Fixture::new();
    let mock = Mock::default();
    let config = ElevationOptions {
        provider: ElevationProvider::Mapbox,
        mapbox_zoom: 8,
        ..Default::default()
    };
    let grid = acquire(&config, &fixture.0, &mock).unwrap();
    assert!(grid.heights_m.iter().flatten().all(|h| *h == Some(1000.0)));
    assert!(grid.vertical_datum.starts_with("mixed:"));
    assert!(grid.resolution_m > 500.0); // z8 pixels must not be advertised as 30m.
    assert_eq!(mock.calls.borrow().len(), 1);
    assert_eq!(tile_pixel(0.0, 0.0, 1), (1, 1, 0, 0));
}

#[test]
fn network_errors_never_contain_request_token_or_url() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = std::thread::spawn(move || {
        use std::io::Write;
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = [0u8; 4096];
        let mut received = Vec::new();
        while !received.windows(4).any(|w| w == b"\r\n\r\n") {
            let count = stream.read(&mut request).unwrap();
            assert!(count > 0);
            received.extend_from_slice(&request[..count]);
            assert!(received.len() < 16384);
        }
        stream
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
    });
    let transport = HttpTransport::new(&ElevationOptions::default());
    let error = transport
        .bytes(transport.agent.get(&format!(
            "http://{address}/tile?access_token=fixture-secret"
        )))
        .unwrap_err();
    assert_eq!(error, "HTTP 401");
    assert!(!error.contains("fixture-secret"));
    task.join().unwrap();
}
