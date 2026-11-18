//! 请求头明文构造。
//! Android 按官方 `CommonEncryptionInterceptor` 的 `HashMap + Gson` 语义输出：
//! 全字符串值、Java HashMap 迭代序、Gson HTML 安全转义；appInstallTime/appUpdateTime
//! 从设备身份读取，同一 DeviceId 全生命周期稳定（逐请求漂移会影响设备一致性）。

use super::envelope::{md5_hex, now_ms};

/// tokenSign 固定后缀（与 Android 签名盐同源）。
pub const TOKEN_SIGN_SUFFIX: &str = super::sign::SALT;

pub const IOS_APP_VERSION: &str = "7.3.40";
pub const ANDROID_APP_VERSION: &str = "7.3.93";

pub const UA_IOS: &str = "SWCampus/7.3.40 (iPhone; iOS 18.1; Scale/3.00)";
pub const UA_ANDROID: &str = "Mozilla/5.0 (Linux; Android 14; 22081212C Build/UKQ1.231003.002) AppleWebKit/537.36 SWCampus/7.3.93";

pub fn app_version_for(identity: &HeaderIdentity) -> &'static str {
    if identity.platform == "android" {
        ANDROID_APP_VERSION
    } else {
        IOS_APP_VERSION
    }
}

pub fn ua_for(identity: &HeaderIdentity) -> &'static str {
    if identity.platform == "android" {
        UA_ANDROID
    } else {
        UA_IOS
    }
}

type HeaderFields = Vec<(String, String)>;

pub fn content_type_for(identity: &HeaderIdentity) -> &'static str {
    if identity.platform == "android" {
        "application/json"
    } else {
        "application/json; charset=utf-8"
    }
}

/// 设备身份。app_install_time <= 0 表示尚未生成。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HeaderIdentity {
    #[serde(default = "default_platform")]
    pub platform: String, // "ios" | "android"
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub idfa: String,
    #[serde(default = "default_os_version")]
    pub os_version: String,
    #[serde(default = "default_device_name")]
    pub device_name: String,
    /// 本地展示的制造商品牌；不改变服务端 deviceName（机型）字段。
    #[serde(default)]
    pub manufacturer: String,
    /// Android `logicPixel`（dp 宽x高）。为空时用 393x873。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub logic_pixel: String,
    /// Android `physicPixel`（px 宽x高）。为空时用 1080x2400。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub physic_pixel: String,
    /// Android `cpuModel`（Build.SUPPORTED_ABIS[0]）。为空时用 arm64-v8a。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cpu_model: String,
    /// Android `isRoot`，字符串 "0"/"1"。为空时用 "0"。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub is_root: String,
    #[serde(default = "default_anchor_lat")]
    pub anchor_lat: f64,
    #[serde(default = "default_anchor_lon")]
    pub anchor_lon: f64,
    #[serde(default)]
    pub app_install_time: i64,
    /// 设备 MAC（首次生成后持久化，避免写死指纹）
    #[serde(default)]
    pub mac_address: String,
    /// 提交 body 的城市名
    #[serde(default = "default_city")]
    pub city: String,
}

pub fn random_mac() -> String {
    // 本地管理位随机单播 MAC
    let mut rng = rand::thread_rng();
    let octets: [u8; 3] = [0x08, 0xa4, rand::Rng::gen(&mut rng)];
    let tail: [u8; 3] = rand::Rng::gen(&mut rng);
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        octets[0], octets[1], octets[2], tail[0], tail[1], tail[2]
    )
}

fn default_platform() -> String {
    "ios".into()
}
fn default_os_version() -> String {
    "26.5.2".into()
}
fn default_device_name() -> String {
    "iPhone".into()
}
fn default_anchor_lat() -> f64 {
    38.901678
}
fn default_anchor_lon() -> f64 {
    121.540241
}
fn default_city() -> String {
    "大连市".into()
}

impl HeaderIdentity {
    /// Generate one independent device environment, keeping the configured real
    /// location. Save once per profile; ordinary loads must never call this.
    #[cfg(any(feature = "gui", test))]
    pub fn independent_device(&self, platform: &str) -> Self {
        use rand::{seq::SliceRandom, Rng};
        let mut rng = rand::thread_rng();
        let platform = if platform == "android" {
            "android"
        } else {
            "ios"
        };
        let mut identity = Self {
            platform: platform.into(),
            device_id: uuid::Uuid::new_v4().to_string().to_uppercase(),
            mac_address: random_mac(),
            app_install_time: Self::fresh_install_time(platform)
                - rng.gen_range(0..7 * 86_400_000_i64),
            city: self.city.clone(),
            anchor_lat: self.anchor_lat,
            anchor_lon: self.anchor_lon,
            ..Self::default()
        };
        let versions: &[&str] = if platform == "android" {
            &["12", "13", "14", "15"]
        } else {
            &["17.5.1", "18.0.1", "18.1", "18.2", "26.5.2"]
        };
        let alternatives: Vec<_> = versions
            .iter()
            .copied()
            .filter(|v| *v != self.os_version)
            .collect();
        identity.os_version = alternatives.choose(&mut rng).unwrap().to_string();
        if platform == "android" {
            let models = [
                "22081212C",
                "OPPO PGBM10",
                "Redmi K60",
                "HUAWEI Mate 40",
                "vivo V2309A",
                "Pixel 8",
                "SM-S9210",
                "OnePlus ACE3",
            ];
            let alternatives: Vec<_> = models
                .into_iter()
                .filter(|m| *m != self.device_name)
                .collect();
            identity.device_name = alternatives.choose(&mut rng).unwrap().to_string();
            identity.logic_pixel = "393x873".into();
            identity.physic_pixel = "1080x2400".into();
            identity.cpu_model = "arm64-v8a".into();
            identity.is_root = "0".into();
        } else {
            identity.device_name = "iPhone".into();
            identity.idfa = uuid::Uuid::new_v4().to_string().to_uppercase();
        }
        identity
    }

    /// 返回设备页配置的锚点，并执行统一边界校验。
    pub fn anchor_coordinate(&self) -> Result<crate::location::Coordinate, String> {
        crate::location::Coordinate::new(self.anchor_lat, self.anchor_lon, 0.0)
    }

    /// 旧版本首次启动会写入大连默认值。未明确配置前禁止将它作为异地跑步位置使用。
    /// 默认值以锚点为准：城市本身是大连的学校，改过锚点即视为已配置。
    pub fn has_unconfigured_default_location(&self) -> bool {
        self.city.trim().is_empty()
            || self
                .anchor_coordinate()
                .map(|c| c.is_default_dalian())
                .unwrap_or(true)
    }

    /// 安装时间：持久值优先；缺失时按平台惯例回退（iOS 3 天 / Android 90 天前）。
    pub fn install_time(&self, ts: i64) -> i64 {
        if self.app_install_time > 0 {
            self.app_install_time
        } else if self.platform == "android" {
            ts - 90 * 86_400_000
        } else {
            ts - 3 * 86_400_000
        }
    }

    /// 本次请求使用的 DeviceId：持久值优先；缺失时（仅异常路径）生成一次随机值。
    /// 调用方每个请求只取一次，header 与 body 共用同一值。
    pub fn request_device_id(&self) -> String {
        if self.device_id.is_empty() {
            uuid::Uuid::new_v4().to_string().to_uppercase()
        } else {
            self.device_id.clone()
        }
    }

    /// 按平台生成一份新的安装时间（设备随机化时调用）。
    pub fn fresh_install_time(platform: &str) -> i64 {
        let days = if platform == "android" { 90 } else { 3 };
        now_ms() - days * 86_400_000
    }
}

impl Default for HeaderIdentity {
    fn default() -> Self {
        Self {
            platform: default_platform(),
            device_id: String::new(),
            idfa: String::new(),
            os_version: default_os_version(),
            device_name: default_device_name(),
            manufacturer: String::new(),
            logic_pixel: String::new(),
            physic_pixel: String::new(),
            cpu_model: String::new(),
            is_root: String::new(),
            anchor_lat: default_anchor_lat(),
            anchor_lon: default_anchor_lon(),
            app_install_time: 0,
            mac_address: String::new(),
            city: default_city(),
        }
    }
}

/// SWEncryption.signObject:suffix: 的 tokenSign。
/// SWJSON.sortedQueryString 按 key 排序：`timeStamp=<ts>&token=<token>&uid=<uid>`。
pub fn native_token_sign(uid: i64, token: &str, timestamp_ms: i64) -> String {
    let query = format!("timeStamp={}&token={}&uid={}", timestamp_ms, token, uid);
    md5_hex(format!("{}{}", query, TOKEN_SIGN_SUFFIX).as_bytes())
}

/// 官方 `R.string.md5_sign_salt`，仅 `datacollection/verify` 的 lite 头使用。
pub const LITE_TOKEN_SIGN_SALT: &str = "&wh2016_swcampus";
pub const VERIFY_LITE_PATH: &str = "api/v76/datacollection/verify";

fn lite_token_sign(uid: i64, token: &str, timestamp_ms: i64) -> String {
    let query = format!("timeStamp={}&token={}&uid={}", timestamp_ms, token, uid);
    md5_hex(format!("{}{}", query, LITE_TOKEN_SIGN_SALT).as_bytes())
}

fn insert_uuid(map: &mut serde_json::Map<String, serde_json::Value>) -> String {
    let u = uuid::Uuid::new_v4().to_string().to_uppercase();
    map.insert("nonce".into(), serde_json::Value::String(u.clone()));
    u
}

/// iOS 请求头明文（SWRequestHeaderProvider._fillFullHeaders 字段序）。
/// 返回 (紧凑 JSON, HTTP 附加头字段: nonce/tokenSign/timeStamp)。
pub fn build_native_ios_header(
    identity: &HeaderIdentity,
    uid: i64,
    token: &str,
    timestamp_ms: Option<i64>,
) -> (String, Vec<(String, String)>) {
    let mut m = serde_json::Map::new();
    let device_id = identity.request_device_id();
    let ts = timestamp_ms.unwrap_or_else(now_ms);
    let install = identity.install_time(ts);

    m.insert("osType".into(), serde_json::Value::String("1".into()));
    m.insert(
        "DeviceId".into(),
        serde_json::Value::String(device_id.clone()),
    );
    m.insert(
        "deviceName".into(),
        serde_json::Value::String(identity.device_name.clone()),
    );
    m.insert(
        "CustomDeviceId".into(),
        serde_json::Value::String(format!("{device_id}_iOS_sportsWorld_campus")),
    );
    m.insert(
        "osVersion".into(),
        serde_json::Value::String(identity.os_version.clone()),
    );
    if !identity.idfa.is_empty() {
        m.insert(
            "IDFA".into(),
            serde_json::Value::String(identity.idfa.clone()),
        );
    }
    m.insert(
        "logicPixel".into(),
        serde_json::Value::String("360x640".into()),
    );
    m.insert(
        "physicPixel".into(),
        serde_json::Value::String("1080x1920".into()),
    );
    m.insert(
        "cpuModel".into(),
        serde_json::Value::String("x86_64".into()),
    );
    m.insert(
        "appVersion".into(),
        serde_json::Value::String(IOS_APP_VERSION.into()),
    );
    m.insert("isRoot".into(), serde_json::Value::Bool(false));
    m.insert("appInstallTime".into(), serde_json::Value::from(install));
    let nonce = insert_uuid(&mut m);
    if uid >= 1 {
        m.insert("uid".into(), serde_json::Value::from(uid));
    }
    if !token.is_empty() {
        m.insert("token".into(), serde_json::Value::String(token.to_string()));
    }
    m.insert("timeStamp".into(), serde_json::Value::from(ts));
    m.insert(
        "studentId".into(),
        serde_json::Value::from(if uid >= 1 { uid } else { 0 }),
    );
    if uid >= 1 && !token.is_empty() {
        m.insert(
            "tokenSign".into(),
            serde_json::Value::String(native_token_sign(uid, token, ts)),
        );
    }
    let extra = vec![
        ("nonce".to_string(), nonce),
        ("timeStamp".to_string(), ts.to_string()),
        ("tokenSign".to_string(), native_token_sign(uid, token, ts)),
    ];
    (serde_json::to_string(&m).unwrap_or_default(), extra)
}

/// Android 请求头字段（官方 `HttpUtil.getHeaderMap` 插入序；值全部为字符串）。
///
/// 官方 `CommonEncryptionInterceptor` 把这些 header 收进 `HashMap` 后用 Gson
/// 序列化；这里的返回顺序只用于 HashMap 冲突时的插入序，最终 JSON 由
/// [`serialize_header_fields`] 按 Java HashMap 迭代序输出。
fn android_header_fields(
    identity: &HeaderIdentity,
    uid: i64,
    token: &str,
    timestamp_ms: Option<i64>,
) -> (HeaderFields, HeaderFields) {
    let device_id = identity.request_device_id();
    let ts = timestamp_ms.unwrap_or_else(now_ms);
    let install = identity.install_time(ts).to_string();
    // 官方 Utils.Oooo0o(): UUID.randomUUID().toString()，小写。
    let nonce = uuid::Uuid::new_v4().to_string();
    // 官方 getHeaderMap(): token 为空或 uid<0 时 tokenSign=""。
    let sign = if uid >= 0 && !token.is_empty() {
        native_token_sign(uid, token, ts)
    } else {
        String::new()
    };

    let fields = vec![
        ("Accept".into(), "application/json".into()),
        ("Content-Type".into(), "application/json".into()),
        ("appVersion".into(), ANDROID_APP_VERSION.into()),
        ("osType".into(), "0".into()),
        ("DeviceId".into(), device_id),
        ("osVersion".into(), identity.os_version.clone()),
        ("deviceName".into(), identity.device_name.clone()),
        ("IMEI".into(), String::new()),
        (
            "logicPixel".into(),
            if identity.logic_pixel.is_empty() {
                "393x873".to_string()
            } else {
                identity.logic_pixel.clone()
            },
        ),
        (
            "physicPixel".into(),
            if identity.physic_pixel.is_empty() {
                "1080x2400".to_string()
            } else {
                identity.physic_pixel.clone()
            },
        ),
        ("androidId".into(), String::new()),
        ("blMac".into(), String::new()),
        ("wifiMac".into(), String::new()),
        (
            "cpuModel".into(),
            if identity.cpu_model.is_empty() {
                "arm64-v8a".to_string()
            } else {
                identity.cpu_model.clone()
            },
        ),
        (
            "isRoot".into(),
            if identity.is_root.is_empty() {
                "0".to_string()
            } else {
                identity.is_root.clone()
            },
        ),
        ("appUpdateTime".into(), install.clone()),
        ("appInstallTime".into(), install),
        ("nonce".into(), nonce.clone()),
        ("timeStamp".into(), ts.to_string()),
        ("CustomDeviceId".into(), String::new()),
        ("uid".into(), uid.to_string()),
        ("token".into(), token.to_string()),
        ("tokenSign".into(), sign.clone()),
    ];
    let extra = vec![
        ("nonce".to_string(), nonce),
        ("timeStamp".to_string(), ts.to_string()),
        ("tokenSign".to_string(), sign),
    ];
    (fields, extra)
}

/// 官方 `NativeNetworkFlutterParamHeaders.verifyLiteHeaders` 字段集。
fn android_lite_header_fields(
    identity: &HeaderIdentity,
    uid: i64,
    token: &str,
    timestamp_ms: Option<i64>,
) -> (HeaderFields, HeaderFields) {
    let ts = timestamp_ms.unwrap_or_else(now_ms);
    let nonce = uuid::Uuid::new_v4().to_string();
    let sign = if uid >= 0 && !token.is_empty() {
        lite_token_sign(uid, token, ts)
    } else {
        String::new()
    };
    let fields = vec![
        ("Accept".into(), "application/json".into()),
        ("Content-Type".into(), "application/json".into()),
        ("appVersion".into(), ANDROID_APP_VERSION.into()),
        ("DeviceId".into(), identity.request_device_id()),
        ("osVersion".into(), identity.os_version.clone()),
        ("deviceName".into(), identity.device_name.clone()),
        ("osType".into(), "0".into()),
        ("uid".into(), uid.to_string()),
        ("token".into(), token.to_string()),
        ("timeStamp".into(), ts.to_string()),
        ("tokenSign".into(), sign.clone()),
        ("nonce".into(), nonce.clone()),
    ];
    let extra = vec![
        ("nonce".to_string(), nonce),
        ("timeStamp".to_string(), ts.to_string()),
        ("tokenSign".to_string(), sign),
    ];
    (fields, extra)
}

/// 官方 `HashMap` 的迭代顺序：桶下标升序，同桶按插入序。
///
/// 只模拟 `String` 键、默认容量 16、负载因子 0.75；header 数量远低于
/// treeify 阈值，不处理红黑树桶。
fn java_string_hash(value: &str) -> u32 {
    value
        .encode_utf16()
        .fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(u32::from(c)))
}

fn java_hashmap_order(fields: &[(String, String)]) -> Vec<usize> {
    let mut capacity = 16usize;
    while fields.len() > capacity * 3 / 4 {
        capacity *= 2;
    }
    let mask = capacity as u32 - 1;
    let mut order: Vec<usize> = (0..fields.len()).collect();
    order.sort_by_key(|&i| {
        let hash = java_string_hash(&fields[i].0);
        (hash ^ (hash >> 16)) & mask
    });
    order
}

/// Gson 默认的 HTML-safe 字符串转义。
fn gson_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '=' => out.push_str("\\u003d"),
            '\'' => out.push_str("\\u0027"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn serialize_header_fields(fields: &[(String, String)]) -> String {
    let order = java_hashmap_order(fields);
    let mut out = String::with_capacity(512);
    out.push('{');
    for (pos, &i) in order.iter().enumerate() {
        if pos > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&gson_escape(&fields[i].0));
        out.push_str("\":\"");
        out.push_str(&gson_escape(&fields[i].1));
        out.push('"');
    }
    out.push('}');
    out
}

/// Android `headerSign` 明文（无额外请求头）。
pub fn build_android_header(
    identity: &HeaderIdentity,
    uid: i64,
    token: &str,
    timestamp_ms: Option<i64>,
) -> (String, Vec<(String, String)>) {
    let (fields, extra) = android_header_fields(identity, uid, token, timestamp_ms);
    (serialize_header_fields(&fields), extra)
}

/// Android `headerSign` 明文，包含调用方额外 header（如 Authorization/runes/runef）。
///
/// 官方拦截器会把**请求上的全部 header** 收进 HashMap 后加密，额外 header 不能漏。
pub fn build_android_header_for_request(
    identity: &HeaderIdentity,
    uid: i64,
    token: &str,
    timestamp_ms: Option<i64>,
    extra_headers: &[(String, String)],
) -> (String, Vec<(String, String)>) {
    let (mut fields, extra) = android_header_fields(identity, uid, token, timestamp_ms);
    for (key, value) in extra_headers {
        if let Some(slot) = fields.iter_mut().find(|(name, _)| name == key) {
            slot.1 = value.clone();
        } else {
            fields.push((key.clone(), value.clone()));
        }
    }
    (serialize_header_fields(&fields), extra)
}

/// `datacollection/verify` 专用 lite 头：字段更少，tokenSign 用资源盐。
pub fn build_android_lite_header_for_request(
    identity: &HeaderIdentity,
    uid: i64,
    token: &str,
    timestamp_ms: Option<i64>,
    extra_headers: &[(String, String)],
) -> (String, Vec<(String, String)>) {
    let (mut fields, extra) = android_lite_header_fields(identity, uid, token, timestamp_ms);
    for (key, value) in extra_headers {
        if let Some(slot) = fields.iter_mut().find(|(name, _)| name == key) {
            slot.1 = value.clone();
        } else {
            fields.push((key.clone(), value.clone()));
        }
    }
    (serialize_header_fields(&fields), extra)
}

/// 生成 headerSign 明文，并把调用方额外 header 纳入 Android 的 header 集合。
pub fn build_header_for_request(
    identity: &HeaderIdentity,
    uid: i64,
    token: &str,
    timestamp_ms: Option<i64>,
    extra_headers: &[(String, String)],
) -> (String, Vec<(String, String)>) {
    if identity.platform == "android" {
        if extra_headers.is_empty() {
            build_android_header(identity, uid, token, timestamp_ms)
        } else {
            build_android_header_for_request(identity, uid, token, timestamp_ms, extra_headers)
        }
    } else {
        build_native_ios_header(identity, uid, token, timestamp_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测向量。
    #[test]
    fn test_token_sign_vector() {
        assert_eq!(
            native_token_sign(13056447, "TOKENABC", 1788958186123),
            "4a8d163186d91ac7b539a7bb07d457ba"
        );
    }

    /// iOS 头键序与条件字段（缺省字段跳过）。
    #[test]
    fn test_ios_header_shape() {
        let identity = HeaderIdentity {
            device_id: "0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A".into(),
            idfa: String::new(),
            ..Default::default()
        };
        let (json, _) = build_native_ios_header(&identity, -1, "", Some(1788958186123));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "osType",
                "DeviceId",
                "deviceName",
                "CustomDeviceId",
                "osVersion",
                "logicPixel",
                "physicPixel",
                "cpuModel",
                "appVersion",
                "isRoot",
                "appInstallTime",
                "nonce",
                "timeStamp",
                "studentId"
            ]
        );
        assert_eq!(v["osType"], "1");
        assert_eq!(v["studentId"], 0);
        // 带 uid/token 时出现 tokenSign
        let (json2, _) = build_native_ios_header(&identity, 13056447, "TOKEN", Some(1788958186123));
        let v2: serde_json::Value = serde_json::from_str(&json2).unwrap();
        assert!(v2.get("tokenSign").is_some());
        assert_eq!(v2["uid"], 13056447);
        assert_eq!(
            v2["tokenSign"],
            native_token_sign(13056447, "TOKEN", 1788958186123)
        );
    }

    /// Android 头键集、字符串类型与官方 HashMap/Gson 迭代序。
    #[test]
    fn test_android_header_shape() {
        let identity = HeaderIdentity {
            platform: "android".into(),
            device_id: "D0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A".into(),
            os_version: "14".into(),
            device_name: "22081212C".into(),
            ..Default::default()
        };
        let ts = 1_788_958_186_123_i64;
        let (json, extra) = build_android_header(&identity, 42, "TK", Some(ts));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["osType"], "0");
        assert_eq!(v["appVersion"], "7.3.93");
        assert_eq!(v["CustomDeviceId"], "");
        assert_eq!(v["uid"], "42");
        assert_eq!(v["timeStamp"], ts.to_string());
        assert_eq!(v["appInstallTime"], identity.install_time(ts).to_string());
        assert_eq!(v["logicPixel"], "393x873");
        assert_eq!(v["physicPixel"], "1080x2400");
        assert_eq!(v["cpuModel"], "arm64-v8a");
        assert_eq!(v["isRoot"], "0");
        assert!(v.get("studentId").is_none());
        assert_eq!(v["tokenSign"], native_token_sign(42, "TK", ts));
        assert_eq!(
            v["nonce"].as_str().unwrap(),
            v["nonce"].as_str().unwrap().to_lowercase()
        );
        assert_eq!(extra.last().map(|(k, _)| k.as_str()), Some("tokenSign"));

        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "physicPixel",
                "appVersion",
                "Accept",
                "isRoot",
                "DeviceId",
                "appInstallTime",
                "cpuModel",
                "IMEI",
                "CustomDeviceId",
                "deviceName",
                "appUpdateTime",
                "nonce",
                "token",
                "timeStamp",
                "uid",
                "blMac",
                "osVersion",
                "osType",
                "tokenSign",
                "wifiMac",
                "androidId",
                "Content-Type",
                "logicPixel",
            ]
        );
    }

    /// 额外请求头必须进 headerSign；Gson 的 `=` 转义不能交给 serde。
    #[test]
    fn test_android_header_includes_extra_headers_with_gson_escaping() {
        let identity = HeaderIdentity {
            platform: "android".into(),
            device_id: "D0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A".into(),
            os_version: "14".into(),
            device_name: "22081212C".into(),
            ..Default::default()
        };
        let extras = vec![
            (
                "Authorization".to_string(),
                "Basic dXNlcjpwYXNz=".to_string(),
            ),
            ("runes".to_string(), "1700000000".to_string()),
        ];
        let (json, _) =
            build_android_header_for_request(&identity, 42, "TK", Some(1_788_958_186_123), &extras);
        assert!(json.contains("\"Authorization\":\"Basic dXNlcjpwYXNz\\u003d\""));
        assert!(json.contains("\"runes\":\"1700000000\""));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["Authorization"], "Basic dXNlcjpwYXNz=");
    }

    /// 提交特有的 runes/runef 会把 HashMap 扩容到 64 桶，顺序必须同步变化。
    #[test]
    fn test_android_submit_header_order_includes_runes_and_runef() {
        let identity = HeaderIdentity {
            platform: "android".into(),
            device_id: "D0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A".into(),
            os_version: "14".into(),
            device_name: "22081212C".into(),
            ..Default::default()
        };
        let extras = vec![
            ("runes".to_string(), "1700000000".to_string()),
            ("runef".to_string(), "UUID1700000000".to_string()),
        ];
        let (json, _) =
            build_android_header_for_request(&identity, 42, "TK", Some(1_788_958_186_123), &extras);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "appVersion",
                "isRoot",
                "runes",
                "DeviceId",
                "cpuModel",
                "CustomDeviceId",
                "deviceName",
                "uid",
                "runef",
                "osVersion",
                "osType",
                "androidId",
                "Content-Type",
                "physicPixel",
                "Accept",
                "appInstallTime",
                "IMEI",
                "appUpdateTime",
                "nonce",
                "token",
                "timeStamp",
                "blMac",
                "tokenSign",
                "wifiMac",
                "logicPixel",
            ]
        );
    }

    /// verify 专用 lite 头：字段集、资源盐和 HashMap 顺序。
    #[test]
    fn test_android_verify_lite_header_shape() {
        let identity = HeaderIdentity {
            platform: "android".into(),
            device_id: "D0FA3EA16-46F0-4E44-9A97-1B63F0D9B11A".into(),
            os_version: "14".into(),
            device_name: "22081212C".into(),
            ..Default::default()
        };
        let ts = 1_788_958_186_123_i64;
        let (json, _) = build_android_lite_header_for_request(&identity, 42, "TK", Some(ts), &[]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["tokenSign"], "dcd07960cfdbcf1315e0a63062ad63e0");
        assert_eq!(v["uid"], "42");
        assert_eq!(v["osType"], "0");
        assert!(v.get("IMEI").is_none());
        assert!(v.get("logicPixel").is_none());
        assert!(v.get("CustomDeviceId").is_none());
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "timeStamp",
                "uid",
                "appVersion",
                "Accept",
                "osVersion",
                "DeviceId",
                "osType",
                "tokenSign",
                "deviceName",
                "nonce",
                "Content-Type",
                "token",
            ]
        );
    }

    /// 未登录时官方 tokenSign 为空串，而不是拿 -1 去签名。
    #[test]
    fn test_android_header_anonymous_token_sign_is_empty() {
        let identity = HeaderIdentity {
            platform: "android".into(),
            ..Default::default()
        };
        let (json, extra) = build_android_header(&identity, -1, "", Some(1_788_958_186_123));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["uid"], "-1");
        assert_eq!(v["token"], "");
        assert_eq!(v["tokenSign"], "");
        assert_eq!(extra[2].1, "");
    }

    /// 同一身份的 appInstallTime 跨请求零漂移（设备一致性防护）。
    #[test]
    fn test_install_time_stable_across_requests() {
        let identity = HeaderIdentity {
            app_install_time: 1_700_000_000_000_i64,
            ..Default::default()
        };
        let (j1, _) = build_native_ios_header(&identity, 1, "T", Some(1788958186123));
        let (j2, _) = build_native_ios_header(&identity, 1, "T", Some(1788958187000));
        let v1: serde_json::Value = serde_json::from_str(&j1).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&j2).unwrap();
        assert_eq!(v1["appInstallTime"], v2["appInstallTime"]);
        assert_eq!(v1["appInstallTime"], 1_700_000_000_000_i64);
        // Android appUpdateTime == appInstallTime，同样稳定
        let (a1, _) = build_android_header(&identity, 1, "T", Some(1788958186123));
        let (a2, _) = build_android_header(&identity, 1, "T", Some(1788958190000));
        let a1: serde_json::Value = serde_json::from_str(&a1).unwrap();
        let a2: serde_json::Value = serde_json::from_str(&a2).unwrap();
        assert_eq!(a1["appInstallTime"], a2["appInstallTime"]);
        assert_eq!(a1["appUpdateTime"], a1["appInstallTime"]);
        // 身份未持久化安装时间时，回退值在同一请求内至少自洽
        let fallback = HeaderIdentity::default();
        assert_eq!(fallback.install_time(1000), 1000 - 3 * 86_400_000);
    }

    #[test]
    fn legacy_dalian_defaults_are_not_accepted_for_a_run() {
        assert!(HeaderIdentity::default().has_unconfigured_default_location());
        let configured = HeaderIdentity {
            city: "石家庄市".into(),
            anchor_lat: 38.0428,
            anchor_lon: 114.5149,
            ..Default::default()
        };
        assert!(!configured.has_unconfigured_default_location());
        // A real Dalian school keeps the default city but sets its own anchor.
        let dalian_school = HeaderIdentity {
            city: crate::location::default_city().into(),
            anchor_lat: 38.972493,
            anchor_lon: 121.527121,
            ..Default::default()
        };
        assert!(!dalian_school.has_unconfigured_default_location());
        // Untouched default anchor stays rejected whatever the city says.
        let default_anchor = HeaderIdentity {
            city: "石家庄市".into(),
            ..Default::default()
        };
        assert!(default_anchor.has_unconfigured_default_location());
    }
}
