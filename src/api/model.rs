//! 数据模型与本地持久化。
//!
//! 持久化文件位于桌面 exe 目录或 Android 应用私有目录：identity.json（设备身份，device_id 固定复用
//! ——严禁每次随机，会触发 10121 风控）、session.json（登录态）、config.json
//! （账号/参数）、points_cache.json（点位缓存）、ai_sports.json（AI 项目列表缓存）。
//! 读写统一经由 [`crate::state`]（版本号、迁移、原子写入、损坏备份、0600）；
//! 下面的 `load_*` / `save_*` 是对状态层的薄封装，签名保持不变。

pub use crate::crypto::header::HeaderIdentity;

pub const HOST: &str = "https://run.gxapp.iydsj.com";
/// 排行榜 / 违规名单域名（信封链与 RUN 相同）。
pub const DISCOVERY: &str = "https://discovery.gxapp.iydsj.com";

/// 登录态（session.json）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Session {
    #[serde(default)]
    pub uid: i64,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub unid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub device_id: String,
    /// 登录响应完整业务数据（我的页展示）
    #[serde(default)]
    pub profile: serde_json::Value,
}

fn default_weight() -> f64 {
    68.0
}

impl Session {
    pub fn is_logged_in(&self) -> bool {
        self.uid >= 1 && !self.token.is_empty()
    }
}

/// UI 参数（config.json）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub school_campus: Option<super::campus::Selection>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub remember: bool,
    #[serde(default = "default_f32")]
    pub dist_min: f32,
    #[serde(default = "default_f32")]
    pub dist_max: f32,
    #[serde(default = "default_pace_min")]
    pub pace_min: f32,
    #[serde(default = "default_pace_max")]
    pub pace_max: f32,
    #[serde(default)]
    pub face_check: bool,
    /// 跑步时覆盖轨迹点 bdA；为空表示使用生成器的海拔曲线。
    #[serde(default)]
    pub manual_altitude: Option<f64>,
    /// 跑步时将生成器海拔曲线映射到此范围；为空表示不使用范围覆盖。
    #[serde(default)]
    pub manual_altitude_range: Option<crate::track::altitude::AltitudeRange>,
    #[serde(default = "default_ai_minutes")]
    pub ai_minutes: i64,
    #[serde(default = "default_ai_reps")]
    pub ai_reps: i64,
    /// 启动时检查更新：silent（静默，发现新版才弹窗）/ ask（每次询问）/ off
    #[serde(default = "default_update_check")]
    pub update_check: String,
    /// OSM 路网文件路径（真实道路路由用）。
    #[serde(default)]
    pub osm_path: String,
    /// 路线算法：legacy | road。
    #[serde(default = "default_route_mode")]
    pub route_mode: String,
}

fn default_route_mode() -> String {
    "legacy".into()
}

fn default_f32() -> f32 {
    1.0
}
fn default_pace_min() -> f32 {
    360.0
}
fn default_pace_max() -> f32 {
    480.0
}
fn default_ai_minutes() -> i64 {
    1
}
fn default_ai_reps() -> i64 {
    5
}
fn default_update_check() -> String {
    "silent".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            school_campus: None,
            username: String::new(),
            password: String::new(),
            remember: false,
            dist_min: 1.0,
            dist_max: 1.5,
            pace_min: default_pace_min(),
            pace_max: default_pace_max(),
            face_check: true,
            manual_altitude: None,
            manual_altitude_range: None,
            ai_minutes: default_ai_minutes(),
            ai_reps: default_ai_reps(),
            update_check: default_update_check(),
            osm_path: String::new(),
            route_mode: default_route_mode(),
        }
    }
}

// ── 持久化（平台数据目录，经由版本化状态层 crate::state）──────────

use crate::state::{self, StateFile};

/// v0：无 version 字段的历史格式，字段演进靠 serde(default)，结构不变。
impl StateFile for Config {
    const FILE_NAME: &'static str = "config.json";
    const CURRENT_VERSION: u32 = 1;
    const SECRET: bool = true;
}

/// v0：无 version 字段的历史格式，结构不变。
impl StateFile for Session {
    const FILE_NAME: &'static str = "session.json";
    const CURRENT_VERSION: u32 = 1;
    const SECRET: bool = true;
}

/// v0：无 version 字段的历史格式，结构不变。
impl StateFile for HeaderIdentity {
    const FILE_NAME: &'static str = "identity.json";
    const CURRENT_VERSION: u32 = 1;
    const SECRET: bool = true;
}

/// 读取状态文件；缺失时用默认值。损坏时状态层已备份原文件，这里记录错误后用默认值
/// （需要把错误交给调用方时用对应的 `try_load_*`）。
fn load_or_default<T: StateFile + Default>() -> T {
    match state::load::<T>() {
        Ok(value) => value.unwrap_or_default(),
        Err(e) => {
            eprintln!("[state] {e}");
            T::default()
        }
    }
}

fn save_state<T: StateFile>(value: &T) -> Result<(), String> {
    state::save(value).map_err(String::from)
}

/// 加载设备身份；device_id / app_install_time 缺失时生成一次并立即落盘，
/// 此后同一设备全生命周期复用（逐请求漂移会影响设备一致性）。
///
/// identity.json 存在但损坏时不会静默换新：原文件另存为 `identity.json.corrupt-<ms>`，
/// 并尽量从残留文本中找回 device_id 等字段；需要把错误展示给用户时用 [`try_load_identity`]。
pub fn load_identity() -> HeaderIdentity {
    load_identity_for_platform(current_platform())
}

/// 与 [`load_identity`] 相同，但 identity.json 损坏时返回错误且不改动任何文件。
#[allow(dead_code)]
pub fn try_load_identity() -> Result<HeaderIdentity, String> {
    state::read::<HeaderIdentity>()?;
    Ok(load_identity())
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else {
        "ios"
    }
}

fn default_identity(platform: &str) -> HeaderIdentity {
    let mut identity = HeaderIdentity::default();
    if platform == "android" {
        identity.platform = "android".into();
        // Generic fallback until the user opts in or fills the fields manually.
        identity.device_name = "Android".into();
        identity.os_version = "16".into();
    }
    identity
}

/// 从损坏（如被截断）的 identity.json 中找回完整的字段值；残缺的值一律丢弃。
fn salvage_identity(text: &str, platform: &str) -> HeaderIdentity {
    fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
        let at = text.find(&format!("\"{key}\""))? + key.len() + 2;
        let rest = text[at..].trim_start().strip_prefix(':')?.trim_start();
        if let Some(s) = rest.strip_prefix('"') {
            let value = &s[..s.find('"')?];
            (!value.contains('\\')).then_some(value)
        } else {
            // 数字必须有终止符，避免采用被截断的半个数字。
            let end = rest.find(|c: char| {
                !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
            })?;
            Some(&rest[..end])
        }
    }
    let Ok(serde_json::Value::Object(mut map)) = serde_json::to_value(default_identity(platform))
    else {
        return default_identity(platform);
    };
    for key in [
        "platform",
        "device_id",
        "idfa",
        "os_version",
        "device_name",
        "manufacturer",
        "mac_address",
        "city",
    ] {
        if let Some(value) = field(text, key) {
            map.insert(key.into(), value.into());
        }
    }
    if let Some(v) = field(text, "app_install_time").and_then(|v| v.parse::<i64>().ok()) {
        map.insert("app_install_time".into(), v.into());
    }
    for key in ["anchor_lat", "anchor_lon"] {
        if let Some(v) = field(text, key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
        {
            map.insert(key.into(), v.into());
        }
    }
    serde_json::from_value(serde_json::Value::Object(map))
        .unwrap_or_else(|_| default_identity(platform))
}

fn load_identity_for_platform(platform: &str) -> HeaderIdentity {
    let path = state::path::<HeaderIdentity>();
    let mut dirty = false;
    // false：原文件无法安全保留时，本次只在内存中使用身份，不改写磁盘。
    let mut persist = true;
    let mut id: HeaderIdentity = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_identity(platform),
        Err(e) => {
            eprintln!(
                "[identity] 读取 {} 失败：{e}；本次不改写该文件",
                path.display()
            );
            persist = false;
            default_identity(platform)
        }
        Ok(raw) => match state::decode::<HeaderIdentity>(&raw) {
            Ok((id, _)) => id,
            Err(error) => {
                // 不静默换新：先完整备份原文件，再从残留文本中找回字段。
                match state::backup_by_copy(&path, &raw, HeaderIdentity::SECRET) {
                    Ok(backup) => {
                        eprintln!(
                            "[identity] identity.json 内容损坏（{error}）；原文件已备份为 {}，已尽量找回 device_id",
                            backup.display()
                        );
                        dirty = true;
                    }
                    Err(e) => {
                        eprintln!("[identity] identity.json 内容损坏（{error}），备份失败（{e}）；本次不改写该文件");
                        persist = false;
                    }
                }
                salvage_identity(&String::from_utf8_lossy(&raw), platform)
            }
        },
    };
    if id.device_id.is_empty() {
        id.device_id = uuid::Uuid::new_v4().to_string().to_uppercase();
        dirty = true;
    }
    if id.app_install_time <= 0 {
        id.app_install_time =
            crate::crypto::header::HeaderIdentity::fresh_install_time(&id.platform);
        dirty = true;
    }
    if id.mac_address.is_empty() {
        id.mac_address = crate::crypto::header::random_mac();
        dirty = true;
    }
    if dirty && persist {
        let _ = save_identity(&id);
    }
    id
}

#[cfg(any(target_os = "android", test))]
#[derive(Debug, serde::Deserialize)]
pub struct DeviceInfo {
    pub manufacturer: String,
    pub model: String,
    pub os_version: String,
}

#[cfg(any(target_os = "android", test))]
pub fn identity_with_device_info(
    identity: &HeaderIdentity,
    info: &DeviceInfo,
) -> Result<HeaderIdentity, String> {
    if info.model.trim().is_empty() || info.os_version.trim().is_empty() {
        return Err("未能读取完整的机型和系统版本，请手动填写".into());
    }
    let mut updated = identity.clone();
    updated.platform = "android".into();
    updated.device_name = info.model.trim().into();
    updated.manufacturer = info.manufacturer.trim().into();
    updated.os_version = info.os_version.trim().into();
    Ok(updated)
}

pub fn save_identity(id: &HeaderIdentity) -> Result<(), String> {
    save_state(id)
}

/// AI 项目列表缓存（ai_sports.json）。
/// v0：裸数组 `[{id,name},…]`；v1：`{"version":1,"items":[…]}`。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct AiSportsCache {
    items: Vec<crate::api::ai::AiSport>,
}

impl StateFile for AiSportsCache {
    const FILE_NAME: &'static str = "ai_sports.json";
    const CURRENT_VERSION: u32 = 1;
    const SECRET: bool = false;

    fn migrate(from: u32, value: serde_json::Value) -> Result<serde_json::Value, String> {
        match (from, value) {
            (0, serde_json::Value::Array(items)) => Ok(serde_json::json!({ "items": items })),
            (0, other) => Err(format!("期望 JSON 数组，实际为 {other}")),
            (_, value) => Ok(value),
        }
    }
}

/// AI 项目列表永久缓存：拉取成功即落盘，网络异常时兜底展示。
pub fn load_ai_sports() -> Option<Vec<crate::api::ai::AiSport>> {
    match state::load::<AiSportsCache>() {
        Ok(cache) => cache.map(|c| c.items),
        Err(e) => {
            eprintln!("[state] {e}");
            None
        }
    }
}

pub fn save_ai_sports(list: &[crate::api::ai::AiSport]) -> Result<(), String> {
    save_state(&AiSportsCache {
        items: list.to_vec(),
    })
}

pub fn load_session() -> Session {
    load_or_default()
}

/// 与 [`load_session`] 相同，但 session.json 损坏时返回错误（原文件已备份）。
#[allow(dead_code)]
pub fn try_load_session() -> Result<Session, String> {
    Ok(state::load::<Session>()?.unwrap_or_default())
}

pub fn save_session(s: &Session) -> Result<(), String> {
    save_state(s)
}

pub fn clear_session() {
    let _ = state::remove::<Session>();
}

pub fn load_config() -> Config {
    load_or_default()
}

/// 与 [`load_config`] 相同，但 config.json 损坏时返回错误（原文件已备份）。
#[allow(dead_code)]
pub fn try_load_config() -> Result<Config, String> {
    Ok(state::load::<Config>()?.unwrap_or_default())
}

pub fn save_config(c: &Config) -> Result<(), String> {
    save_state(c)
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    fn exe_dir() -> std::path::PathBuf {
        crate::platform::data_dir()
    }

    #[test]
    fn android_identity_import_persists_brand_and_reuses_uuid() {
        let directory =
            std::env::temp_dir().join(format!("neko-identity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(directory.clone()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = None);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory);
        let first = load_identity_for_platform("android");
        assert_eq!(first.platform, "android");
        assert_ne!(first.device_name, "iPhone");
        assert!(uuid::Uuid::parse_str(&first.device_id).is_ok());
        let again = load_identity_for_platform("android");
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&again).unwrap()
        );

        let mut existing = first.clone();
        existing.platform = "ios".into();
        existing.device_name = "Existing manual name".into();
        existing.idfa = "manual-value".into();
        let mut legacy = serde_json::to_value(&existing).unwrap();
        legacy.as_object_mut().unwrap().remove("manufacturer");
        std::fs::write(
            exe_dir().join("identity.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();
        let loaded = load_identity_for_platform("android");
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&existing).unwrap(),
            "opening an existing install must not overwrite a manual identity"
        );

        let info: DeviceInfo = serde_json::from_str(
            r#"{"manufacturer":"Example","model":"Phone 16","os_version":"16"}"#,
        )
        .unwrap();
        assert_eq!(info.manufacturer, "Example");
        let imported = identity_with_device_info(&loaded, &info).unwrap();
        assert_eq!(imported.platform, "android");
        assert_eq!(imported.device_name, "Phone 16");
        assert_eq!(imported.os_version, "16");
        let mut expected = serde_json::to_value(&existing).unwrap();
        expected["platform"] = "android".into();
        expected["device_name"] = "Phone 16".into();
        expected["manufacturer"] = "Example".into();
        expected["os_version"] = "16".into();
        assert_eq!(
            serde_json::to_value(&imported).unwrap(),
            expected,
            "import must preserve UUID, MAC, manual IMEI/IDFA, install time and location"
        );
        assert_eq!(
            load_identity().device_name,
            existing.device_name,
            "preview must not save implicitly"
        );
        save_identity(&imported).unwrap();
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(exe_dir().join("identity.json")).unwrap())
                .unwrap();
        assert_eq!(
            stored.get("manufacturer").and_then(|value| value.as_str()),
            Some("Example"),
            "the consented brand must be written to identity.json"
        );
        let reloaded = load_identity();
        assert_eq!(
            serde_json::to_value(&reloaded).unwrap(),
            serde_json::to_value(&imported).unwrap(),
            "brand and identity must survive a fresh load from disk"
        );
        assert_eq!(reloaded.device_id, first.device_id);
        let (header, _) =
            crate::crypto::header::build_header_for(&reloaded, 0, "", Some(1_700_000_000_000));
        let header: serde_json::Value = serde_json::from_str(&header).unwrap();
        assert_eq!(header["DeviceId"], first.device_id);
        assert_eq!(
            header["deviceName"], "Phone 16",
            "brand must not be prepended to the protocol model field"
        );
        assert_eq!(header["osVersion"], "16");
        assert!(
            header.get("manufacturer").is_none() && header.get("brand").is_none(),
            "persisting local brand metadata must not invent new protocol fields"
        );
    }

    #[test]
    fn incomplete_native_device_info_cannot_replace_identity() {
        let identity = HeaderIdentity::default();
        for (model, os_version) in [("", "16"), ("Phone", " ")] {
            let info = DeviceInfo {
                manufacturer: String::new(),
                model: model.into(),
                os_version: os_version.into(),
            };
            assert!(identity_with_device_info(&identity, &info).is_err());
        }
    }

    fn temp_data_dir(tag: &str) -> impl Drop {
        let directory = std::env::temp_dir().join(format!("neko-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(directory.clone()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = None);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        Cleanup(directory)
    }

    fn corrupt_backups() -> Vec<std::path::PathBuf> {
        std::fs::read_dir(exe_dir())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("identity.json.corrupt-")
            })
            .collect()
    }

    #[test]
    fn truncated_identity_keeps_device_id_and_a_backup() {
        let _cleanup = temp_data_dir("identity-truncated");
        let original = HeaderIdentity {
            device_id: "0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A".into(),
            app_install_time: 1_700_000_000_000,
            mac_address: "08:a4:01:02:03:04".into(),
            city: "石家庄市".into(),
            ..Default::default()
        };
        let full = serde_json::to_string_pretty(&original).unwrap();
        // Cut inside the last string value, after device_id and install time were written.
        let cut = full.find("\"city\"").unwrap() + 12;
        let truncated = &full.as_bytes()[..cut];
        let path = exe_dir().join("identity.json");
        std::fs::write(&path, truncated).unwrap();

        assert!(try_load_identity().is_err(), "corruption must be reported");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            truncated,
            "try_load must not touch the file"
        );

        let loaded = load_identity_for_platform("ios");
        assert_eq!(
            loaded.device_id, original.device_id,
            "device id must not be regenerated"
        );
        assert_eq!(loaded.app_install_time, original.app_install_time);
        assert_eq!(loaded.mac_address, original.mac_address);
        let backups = corrupt_backups();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read(&backups[0]).unwrap(),
            truncated,
            "backup keeps original bytes"
        );
        // The repaired file is valid again and stable across loads.
        assert_eq!(try_load_identity().unwrap().device_id, original.device_id);
        assert_eq!(
            load_identity_for_platform("ios").device_id,
            original.device_id
        );
        assert_eq!(corrupt_backups().len(), 1);
    }

    #[test]
    fn unsalvageable_identity_is_backed_up_before_replacement() {
        let _cleanup = temp_data_dir("identity-garbage");
        let path = exe_dir().join("identity.json");
        std::fs::write(&path, b"{\"device_id\":\"ABC").unwrap();
        let loaded = load_identity_for_platform("ios");
        assert!(!loaded.device_id.is_empty() && loaded.device_id != "ABC");
        let backups = corrupt_backups();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), b"{\"device_id\":\"ABC");
    }

    #[test]
    fn state_files_are_written_atomically_and_privately() {
        let _cleanup = temp_data_dir("atomic");
        let dir = exe_dir();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        save_session(&Session {
            uid: 7,
            token: "test-only-token".into(),
            ..Default::default()
        })
        .unwrap();
        save_config(&Config {
            password: "test-only-password".into(),
            ..Default::default()
        })
        .unwrap();
        save_points_cache(
            crate::location::Coordinate::new(39.9, 116.4, 0.0).unwrap(),
            &PointsCacheScope {
                uid: 7,
                unid: "1".into(),
                run_area_id: None,
            },
            &[],
        )
        .unwrap();
        assert_eq!(load_session().uid, 7);
        assert_eq!(load_config().password, "test-only-password");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temporary files must be renamed into place"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |name: &str| {
                std::fs::metadata(dir.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!(
                mode("session.json"),
                0o600,
                "pre-existing world-readable session is tightened"
            );
            assert_eq!(mode("config.json"), 0o600);
        }
    }

    #[test]
    fn session_roundtrip_and_logout_use_private_data_directory() {
        let directory = std::env::temp_dir().join(format!("neko-storage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(directory.clone()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = None);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        assert_eq!(
            exe_dir(),
            directory,
            "storage must use the injected app directory"
        );
        let session = Session {
            uid: 123,
            token: "test-only-token".into(),
            ..Default::default()
        };
        save_session(&session).unwrap();
        assert!(directory.join("session.json").is_file());
        assert_eq!(load_session().uid, 123);
        assert_eq!(load_session().token, session.token);
        clear_session();
        assert!(!directory.join("session.json").exists());
        assert_eq!(load_session().uid, 0);
    }
}

/// 点位缓存：{ts_ms, points}，TTL 300s（服务端限流 10603：5 分钟 3 次）。
pub const POINTS_TTL_MS: i64 = 300_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PointsCacheScope {
    pub uid: i64,
    pub unid: String,
    pub run_area_id: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PointsCache {
    ts: i64,
    anchor: crate::location::Coordinate,
    scope: PointsCacheScope,
    points: Vec<serde_json::Value>,
}

impl PointsCache {
    fn matches(
        &self,
        anchor: crate::location::Coordinate,
        scope: &PointsCacheScope,
        now: i64,
    ) -> bool {
        self.scope == *scope
            && self.anchor.is_near(anchor, 0.0001)
            && !self.points.is_empty()
            && now
                .checked_sub(self.ts)
                .is_some_and(|age| (0..POINTS_TTL_MS).contains(&age))
    }
}

/// v0：无 version 字段。早期 v0 只有 `{ts, points}`（未记录锚点 / 账号 / 学校 / 区域），
/// 迁移时补齐缺失字段并把 ts 置 0，使其作为“已过期缓存”正常读取而不是被当作损坏。
/// 含账号与位置，按密文件（0600）处理。
impl StateFile for PointsCache {
    const FILE_NAME: &'static str = "points_cache.json";
    const CURRENT_VERSION: u32 = 1;
    const SECRET: bool = true;

    fn migrate(from: u32, value: serde_json::Value) -> Result<serde_json::Value, String> {
        if from != 0 {
            return Ok(value);
        }
        let serde_json::Value::Object(mut map) = value else {
            return Err(format!("期望 JSON 对象，实际为 {value}"));
        };
        let legacy = ["ts", "anchor", "scope", "points"]
            .iter()
            .any(|key| !map.contains_key(*key));
        if legacy {
            map.insert("ts".into(), 0.into());
            map.entry("anchor")
                .or_insert_with(|| serde_json::json!({"latitude": 0.0, "longitude": 0.0}));
            map.entry("scope")
                .or_insert_with(|| serde_json::json!({"uid": 0, "unid": "", "run_area_id": null}));
            map.entry("points").or_insert_with(|| serde_json::json!([]));
        }
        Ok(serde_json::Value::Object(map))
    }
}

/// 旧版未记录账号、学校、区域的缓存自然失效；拒绝过期或来自未来的缓存。
pub fn load_points_cache_for(
    anchor: crate::location::Coordinate,
    scope: &PointsCacheScope,
) -> Option<(i64, Vec<serde_json::Value>)> {
    let cache = match state::load::<PointsCache>() {
        Ok(cache) => cache?,
        Err(e) => {
            eprintln!("[state] {e}");
            return None;
        }
    };
    if !cache.matches(anchor, scope, crate::crypto::envelope::now_ms()) {
        return None;
    }
    let ts = cache.ts;
    let pts = cache.points;
    Some((ts, pts))
}

pub fn save_points_cache(
    anchor: crate::location::Coordinate,
    scope: &PointsCacheScope,
    points: &[serde_json::Value],
) -> Result<(), String> {
    let doc = PointsCache {
        ts: crate::crypto::envelope::now_ms(),
        anchor,
        scope: scope.clone(),
        points: points.to_vec(),
    };
    save_state(&doc)
}

#[cfg(test)]
mod points_cache_tests {
    use super::*;
    #[test]
    fn cache_is_scoped_to_anchor() {
        let cache = PointsCache {
            ts: 1,
            anchor: crate::location::Coordinate::new(39.9, 116.4, 0.0).unwrap(),
            scope: PointsCacheScope {
                uid: 1,
                unid: "school".into(),
                run_area_id: None,
            },
            points: vec![serde_json::json!({"lat":39.9,"lon":116.4})],
        };
        assert!(cache.matches(cache.anchor, &cache.scope, 1));
        assert!(cache.matches(cache.anchor, &cache.scope, POINTS_TTL_MS));
        assert!(!cache.matches(cache.anchor, &cache.scope, POINTS_TTL_MS + 1));
        assert!(!cache.matches(cache.anchor, &cache.scope, 0));
        for scope in [
            PointsCacheScope {
                uid: 2,
                ..cache.scope.clone()
            },
            PointsCacheScope {
                unid: "other".into(),
                ..cache.scope.clone()
            },
            PointsCacheScope {
                run_area_id: Some("42".into()),
                ..cache.scope.clone()
            },
        ] {
            assert!(!cache.matches(cache.anchor, &scope, 1));
        }
        assert!(!cache.matches(
            crate::location::Coordinate::new(38.9, 121.5, 0.0).unwrap(),
            &cache.scope,
            1
        ));
        let mut old = serde_json::to_value(&cache).unwrap();
        old.as_object_mut().unwrap().remove("scope");
        assert!(serde_json::from_value::<PointsCache>(old).is_err());
        let decoded: PointsCache =
            serde_json::from_value(serde_json::to_value(cache).unwrap()).unwrap();
        assert!(decoded.anchor.is_near(
            crate::location::Coordinate::new(39.9, 116.4, 0.0).unwrap(),
            0.0001
        ));
        assert!(!decoded.anchor.is_near(
            crate::location::Coordinate::new(38.9, 121.5, 0.0).unwrap(),
            0.0001
        ));
    }
}

/// 每个真实状态文件：当前磁盘格式（v0 夹具）→ 读取 → 保存（带版本号）→ 再读取。
#[cfg(test)]
mod state_file_tests {
    use super::*;
    use crate::state::tests::TempDataDir;
    use serde_json::{json, Value};

    const CONFIG_V0: &str = include_str!("../state/fixtures/config.v0.json");
    const SESSION_V0: &str = include_str!("../state/fixtures/session.v0.json");
    const IDENTITY_V0: &str = include_str!("../state/fixtures/identity.v0.json");
    const POINTS_V0: &str = include_str!("../state/fixtures/points_cache.v0.json");
    const POINTS_LEGACY: &str = include_str!("../state/fixtures/points_cache.legacy.json");
    const AI_SPORTS_V0: &str = include_str!("../state/fixtures/ai_sports.v0.json");

    fn on_disk<T: StateFile>() -> Value {
        serde_json::from_slice(&std::fs::read(state::path::<T>()).unwrap()).unwrap()
    }

    /// 写入夹具 → 加载 → 保存 → 校验版本戳在最前、正文 == `expected_body` → 再加载一致。
    fn roundtrip<T: StateFile>(dir: &TempDataDir, fixture: &str, expected_body: Value) -> T {
        std::fs::write(state::path::<T>(), fixture).unwrap();
        let loaded = state::load::<T>().unwrap().expect("fixture must load");
        state::save(&loaded).unwrap();
        let mut saved = on_disk::<T>();
        let map = saved.as_object_mut().unwrap();
        assert_eq!(map.keys().next().map(String::as_str), Some("version"));
        assert_eq!(map.shift_remove("version"), Some(json!(T::CURRENT_VERSION)));
        assert_eq!(
            saved,
            expected_body,
            "{} body must round-trip",
            T::FILE_NAME
        );
        let again = state::load::<T>().unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&again).unwrap(),
            serde_json::to_value(&loaded).unwrap()
        );
        assert!(dir.backups(T::FILE_NAME).is_empty());
        #[cfg(unix)]
        if T::SECRET {
            assert_eq!(crate::state::tests::mode(&state::path::<T>()), 0o600);
        }
        loaded
    }

    /// 降级兼容：旧版本程序（直接 serde 反序列化、忽略未知字段）仍能读取新文件。
    fn old_build_reads<T: serde::de::DeserializeOwned>(name: &str) -> T {
        serde_json::from_slice(&std::fs::read(crate::platform::data_dir().join(name)).unwrap())
            .unwrap()
    }

    #[test]
    fn config_v0_roundtrip() {
        let dir = TempDataDir::new("config");
        let fixture: Value = serde_json::from_str(CONFIG_V0).unwrap();
        let config: Config = roundtrip(&dir, CONFIG_V0, fixture);
        assert_eq!(config.password, "test-only-password");
        assert_eq!(config.route_mode, "road");
        assert_eq!(config.manual_altitude_range.unwrap().max_m, 60.0);
        assert_eq!(load_config().username, "13800000000");
        assert_eq!(old_build_reads::<Config>("config.json").dist_max, 2.5);
    }

    #[test]
    fn partial_v0_config_still_uses_field_defaults() {
        let _dir = TempDataDir::new("config-partial");
        std::fs::write(
            state::path::<Config>(),
            r#"{"username":"u","password":"p","remember":true}"#,
        )
        .unwrap();
        let config = try_load_config().unwrap();
        assert_eq!(config.username, "u");
        assert_eq!(config.update_check, "silent");
        assert_eq!(config.route_mode, "legacy");
        assert_eq!(config.pace_max, 480.0);
    }

    #[test]
    fn session_v0_roundtrip() {
        let dir = TempDataDir::new("session");
        let fixture: Value = serde_json::from_str(SESSION_V0).unwrap();
        let session: Session = roundtrip(&dir, SESSION_V0, fixture);
        assert!(session.is_logged_in());
        assert_eq!(session.profile["nested"]["list"], json!([1, 2, 3]));
        assert_eq!(load_session().uid, 1_320_403_809);
        assert_eq!(old_build_reads::<Session>("session.json").weight, 60.5);
    }

    #[test]
    fn identity_v0_roundtrip_and_load_identity_does_not_rewrite_a_complete_file() {
        let dir = TempDataDir::new("identity");
        let fixture: Value = serde_json::from_str(IDENTITY_V0).unwrap();
        // 完整的 v0 身份：load_identity 不应改写（无 dirty 字段），版本号随下次保存写入。
        std::fs::write(state::path::<HeaderIdentity>(), IDENTITY_V0).unwrap();
        let id = load_identity_for_platform("ios");
        assert_eq!(
            std::fs::read_to_string(state::path::<HeaderIdentity>()).unwrap(),
            IDENTITY_V0
        );
        assert_eq!(id.platform, "android", "stored platform wins");
        let identity: HeaderIdentity = roundtrip(&dir, IDENTITY_V0, fixture);
        assert_eq!(identity.device_id, "0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A");
        assert_eq!(try_load_identity().unwrap().device_id, identity.device_id);
        assert_eq!(
            old_build_reads::<HeaderIdentity>("identity.json").mac_address,
            "08:a4:01:02:03:04"
        );
    }

    #[test]
    fn points_cache_v0_roundtrip() {
        let dir = TempDataDir::new("points");
        let fixture: Value = serde_json::from_str(POINTS_V0).unwrap();
        let cache: PointsCache = roundtrip(&dir, POINTS_V0, fixture);
        assert_eq!(cache.ts, 1_700_000_000_000);
        assert_eq!(cache.scope.run_area_id.as_deref(), Some("73"));
        assert_eq!(cache.points.len(), 1);
        let old: Value = old_build_reads("points_cache.json");
        assert_eq!(old["scope"]["run_area_id"], "73");
    }

    #[test]
    fn fresh_points_cache_is_served_back() {
        let dir = TempDataDir::new("points-fresh");
        let anchor = crate::location::Coordinate::new(39.9, 116.4, 0.0).unwrap();
        let scope = PointsCacheScope {
            uid: 7,
            unid: "1".into(),
            run_area_id: None,
        };
        let points = vec![json!({"lat": 39.9, "lon": 116.4})];
        save_points_cache(anchor, &scope, &points).unwrap();
        assert_eq!(on_disk::<PointsCache>()["version"], 1);
        let (_, got) = load_points_cache_for(anchor, &scope).unwrap();
        assert_eq!(got, points);
        assert!(dir.backups("points_cache.json").is_empty());
    }

    #[test]
    fn legacy_unscoped_points_cache_migrates_to_an_expired_cache_not_corruption() {
        let dir = TempDataDir::new("points-legacy");
        std::fs::write(state::path::<PointsCache>(), POINTS_LEGACY).unwrap();
        let cache = state::load::<PointsCache>().unwrap().unwrap();
        assert_eq!(cache.ts, 0, "legacy caches are always stale");
        assert_eq!(cache.points.len(), 1, "points are kept, not discarded");
        let anchor = crate::location::Coordinate::new(39.9, 116.4, 0.0).unwrap();
        let scope = PointsCacheScope {
            uid: 0,
            unid: String::new(),
            run_area_id: None,
        };
        assert!(load_points_cache_for(anchor, &scope).is_none());
        assert!(dir.backups("points_cache.json").is_empty());
        assert!(state::path::<PointsCache>().exists());
    }

    #[test]
    fn ai_sports_v0_bare_array_migrates_into_items() {
        let dir = TempDataDir::new("ai");
        let fixture: Value = serde_json::from_str(AI_SPORTS_V0).unwrap();
        let cache: AiSportsCache = roundtrip(&dir, AI_SPORTS_V0, json!({ "items": fixture }));
        assert_eq!(cache.items.len(), 2);
        assert_eq!(cache.items[1].name, "跳绳");
        let list = load_ai_sports().unwrap();
        assert_eq!(list[0].id, 1);
        save_ai_sports(&list).unwrap();
        assert_eq!(on_disk::<AiSportsCache>()["items"][0]["name"], "仰卧起坐");
    }

    #[test]
    fn corrupt_config_and_session_are_backed_up_and_reported() {
        let dir = TempDataDir::new("corrupt");
        let bad_config = br#"{"username":"u","password":"p","dist_min":"#;
        std::fs::write(state::path::<Config>(), bad_config).unwrap();
        let err = try_load_config().unwrap_err();
        assert!(err.contains("config.json.corrupt-"), "{err}");
        let backups = dir.backups("config.json");
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), bad_config);
        #[cfg(unix)]
        assert_eq!(crate::state::tests::mode(&backups[0]), 0o600);
        // 包装函数保持签名：返回默认值，不再重复备份。
        assert_eq!(load_config().username, "");
        assert_eq!(dir.backups("config.json").len(), 1);

        // 合法 JSON 但字段类型错误同样视为损坏。
        std::fs::write(state::path::<Session>(), br#"{"uid":"not a number"}"#).unwrap();
        assert!(!load_session().is_logged_in());
        assert_eq!(dir.backups("session.json").len(), 1);
        assert_eq!(try_load_session().unwrap().uid, 0);

        std::fs::write(state::path::<AiSportsCache>(), b"{\"oops\":1}").unwrap();
        assert!(load_ai_sports().is_none());
        assert_eq!(dir.backups("ai_sports.json").len(), 1);

        std::fs::write(state::path::<PointsCache>(), b"[1,2]").unwrap();
        let anchor = crate::location::Coordinate::new(39.9, 116.4, 0.0).unwrap();
        let scope = PointsCacheScope {
            uid: 0,
            unid: String::new(),
            run_area_id: None,
        };
        assert!(load_points_cache_for(anchor, &scope).is_none());
        assert_eq!(dir.backups("points_cache.json").len(), 1);
    }

    #[test]
    fn corrupt_identity_keeps_copy_backup_semantics() {
        // identity 走专用恢复路径：备份为副本、原文件被找回后的身份覆盖（见 storage_tests）。
        let dir = TempDataDir::new("identity-corrupt");
        let truncated = &IDENTITY_V0.as_bytes()[..IDENTITY_V0.find("\"mac_address\"").unwrap()];
        std::fs::write(state::path::<HeaderIdentity>(), truncated).unwrap();
        assert!(try_load_identity().is_err());
        assert!(
            dir.backups("identity.json").is_empty(),
            "try_load is read-only"
        );
        let id = load_identity_for_platform("ios");
        assert_eq!(id.device_id, "0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A");
        assert_eq!(id.app_install_time, 1_700_000_000_000);
        let backups = dir.backups("identity.json");
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), truncated);
        #[cfg(unix)]
        assert_eq!(crate::state::tests::mode(&backups[0]), 0o600);
        assert_eq!(on_disk::<HeaderIdentity>()["version"], 1);
    }

    #[test]
    fn state_types_do_not_use_the_reserved_version_key() {
        for value in [
            serde_json::to_value(Config::default()).unwrap(),
            serde_json::to_value(Session::default()).unwrap(),
            serde_json::to_value(HeaderIdentity::default()).unwrap(),
            serde_json::to_value(AiSportsCache { items: vec![] }).unwrap(),
        ] {
            assert!(value.get(crate::state::VERSION_KEY).is_none());
        }
    }
}
