//! HTTP 客户端封装。
//!
//! 通用请求封装：body = 信封(明文, insert)，headerSign = 信封(header, observed)，
//! 响应两层解密到业务 JSON；同一实例复用同一 keyDataFour。

use crate::api::model::Session;
use crate::crypto::decrypt::{decrypt_response, derive_paes_key, Decrypted};
use crate::crypto::envelope::{
    build_envelope, build_envelope_ts, rsa_public_key, EnvelopeSession, OuterOrder,
};
use crate::crypto::header::{build_header_for, HeaderIdentity, UA_IOS};

pub fn make_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build()
}

#[derive(Clone)]
pub struct AuthContext {
    pub epoch: u64,
    pub rejected: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub notify: std::sync::mpsc::Sender<AuthFailure>,
}

// 字段只由 GUI 的鉴权失效处理读取（测试读取部分字段）。
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub struct AuthFailure {
    pub epoch: u64,
    pub uid: i64,
    pub token: String,
}

pub struct ApiClient {
    auth_context: Option<AuthContext>,
    auth_rejected: bool,
    #[cfg(test)]
    test_host: Option<String>,
    #[cfg(test)]
    test_response_key: Option<rsa::RsaPublicKey>,
    pub agent: ureq::Agent,
    pub session: EnvelopeSession,
    pub identity: HeaderIdentity,
    pub login: Option<Session>,
}

pub struct RequestOutcome {
    pub http_status: u16,
    pub decrypted: Option<Decrypted>,
    pub raw_len: usize,
    /// 解密失败时的服务端原始返回摘要（诊断用）。
    pub raw_head: String,
}

impl ApiClient {
    pub fn new(identity: HeaderIdentity, login: Option<Session>) -> Self {
        Self {
            auth_context: None,
            auth_rejected: false,
            #[cfg(test)]
            test_host: None,
            #[cfg(test)]
            test_response_key: None,
            agent: make_agent(),
            session: EnvelopeSession::new(),
            identity,
            login,
        }
    }

    #[cfg(any(feature = "gui", test))]
    pub fn with_auth_context(mut self, context: Option<AuthContext>) -> Self {
        self.auth_context = context;
        self
    }

    #[cfg(test)]
    pub(crate) fn set_test_host(&mut self, host: String) {
        assert!(host.starts_with("http://127.0.0.1:"));
        self.test_host = Some(host);
    }

    /// 测试用响应验签公钥（回环服务器用自造私钥签名）。
    #[cfg(test)]
    pub(crate) fn set_test_response_key(&mut self, key: rsa::RsaPublicKey) {
        self.test_response_key = Some(key);
    }

    /// 响应 RSA(s) 验签公钥。
    pub(crate) fn response_key(&self) -> &rsa::RsaPublicKey {
        #[cfg(test)]
        if let Some(key) = &self.test_response_key {
            return key;
        }
        rsa_public_key()
    }

    pub(crate) fn handle_request_error(&mut self, error: ureq::Error) -> String {
        let authenticated = self.login.as_ref().is_some_and(Session::is_logged_in);
        let error = request_error(error, authenticated);
        if is_session_expired(&error) {
            self.auth_rejected = true;
            if let (Some(context), Some(session)) = (&self.auth_context, &self.login) {
                if !context
                    .rejected
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let _ = context.notify.send(AuthFailure {
                        epoch: context.epoch,
                        uid: session.uid,
                        token: session.token.clone(),
                    });
                }
            }
        }
        error
    }

    pub(crate) fn ensure_session_valid(&self) -> Result<(), String> {
        if self.login.as_ref().is_some_and(Session::is_logged_in)
            && (self.auth_rejected
                || self
                    .auth_context
                    .as_ref()
                    .is_some_and(|c| c.rejected.load(std::sync::atomic::Ordering::SeqCst)))
        {
            return Err(SESSION_EXPIRED.into());
        }
        Ok(())
    }

    pub(crate) fn record_url(&self) -> String {
        #[cfg(test)]
        if let Some(host) = &self.test_host {
            return format!(
                "{}{}",
                host.trim_end_matches('/'),
                super::submit::RECORD_PATH
            );
        }
        format!("{}{}", super::model::HOST, super::submit::RECORD_PATH)
    }

    fn uid(&self) -> i64 {
        self.login.as_ref().map(|s| s.uid).unwrap_or(-1)
    }

    pub(crate) fn token(&self) -> String {
        self.login
            .as_ref()
            .map(|s| s.token.clone())
            .unwrap_or_default()
    }

    /// 发送信封请求（header/headerSign 用 observed 序，body 用 insert 序）。
    /// method 为 "POST" 或 "GET"（GET 同样携带信封 body）。
    pub fn envelope_request(
        &mut self,
        method: &str,
        url: &str,
        body_plain: &str,
        ua: &str,
        extra_headers: &[(String, String)],
    ) -> Result<RequestOutcome, String> {
        self.ensure_session_valid()?;
        let uid = self.uid();
        let token = self.token();
        let (header_plain, _) = build_header_for(&self.identity, uid, &token, None);
        let header_env = build_envelope(&mut self.session, &header_plain, OuterOrder::Observed);
        // body 时间戳 = header ts + 1（原生两次独立读取毫秒）
        let body_env = build_envelope_ts(
            &mut self.session,
            body_plain,
            OuterOrder::Insert,
            header_env.ts_ms + 1,
        );

        #[cfg(test)]
        let test_url = self.test_host.as_ref().map(|host| {
            let authority_and_path = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
            let path = authority_and_path
                .find('/')
                .map(|at| &authority_and_path[at..])
                .unwrap_or("/");
            format!("{}{path}", host.trim_end_matches('/'))
        });
        #[cfg(test)]
        let url = test_url.as_deref().unwrap_or(url);
        let mut req = self.agent.request(method, url);
        req = req.set("Content-Type", "application/json; charset=utf-8");
        req = req.set("User-Agent", ua);
        req = req.set("appVersion", "7.3.40");
        req = req.set("headerSign", &header_env.json);
        for (k, v) in extra_headers {
            req = req.set(k, v);
        }
        let resp = req
            .send_string(&body_env.json)
            .map_err(|e| self.handle_request_error(e))?;
        let status = resp.status();
        // 读取失败（含 ureq 10 MiB 上限）如实返回，不伪装成“响应不是合法 JSON”。
        let raw = read_body(resp)?;
        let raw_len = raw.len();
        let key = derive_paes_key(
            &body_env.key_data[0],
            &body_env.key_data[1],
            &body_env.key_data[2],
            &body_env.key_data[3],
        );
        let (decrypted, raw_head) =
            match decrypt_response(raw.as_bytes(), &key, self.response_key()) {
                Ok(d) => (Some(d), String::new()),
                Err(e) => (
                    None,
                    format!("{e} | 原文: {}", crate::textlog::truncate(&raw, 160)),
                ),
            };
        Ok(RequestOutcome {
            http_status: status,
            decrypted,
            raw_len,
            raw_head,
        })
    }

    /// 标准业务请求（RUN 域名）：返回业务 JSON；error != 10000 时 Err。
    pub fn call(
        &mut self,
        method: &str,
        path: &str,
        body_plain: &str,
        extra_headers: &[(String, String)],
    ) -> Result<serde_json::Value, String> {
        self.call_host(
            crate::api::model::HOST,
            method,
            path,
            body_plain,
            extra_headers,
        )
    }

    /// 指定域名业务请求（排行榜/违规名单走 DISCOVERY，信封链相同）。
    /// 默认只记录路径/状态/大小；NSWT_DEBUG_HTTP=1 时追加打码后的业务 JSON。
    pub fn call_host(
        &mut self,
        host: &str,
        method: &str,
        path: &str,
        body_plain: &str,
        extra_headers: &[(String, String)],
    ) -> Result<serde_json::Value, String> {
        let url = format!("{}{}", host, path);
        let out = self.envelope_request(method, &url, body_plain, UA_IOS, extra_headers)?;
        let dec = out.decrypted.ok_or_else(|| {
            format!(
                "HTTP {} 响应解密失败（len={}）原文: {}",
                out.http_status, out.raw_len, out.raw_head
            )
        })?;
        let business = match check_business(dec.business) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[resp] {} -> Err: {}", path, e);
                return Err(e);
            }
        };
        eprintln!("[resp] {} -> {} ({}B)", path, out.http_status, out.raw_len);
        if debug_http_enabled() {
            eprintln!("[resp] {} body {}", path, redact_http_json(&business));
        }
        Ok(business)
    }
}

/// 读取响应正文；读取失败返回真实原因。
pub(crate) fn read_body(resp: ureq::Response) -> Result<String, String> {
    resp.into_string().map_err(|e| format!("读取响应失败: {e}"))
}

/// 完整响应体日志开关（默认关闭；仅本机排错时设置 NSWT_DEBUG_HTTP=1）。
fn debug_http_enabled() -> bool {
    std::env::var_os("NSWT_DEBUG_HTTP").is_some_and(|v| v == "1")
}

/// 调试日志打码：凭证键保留首尾（textlog::redact_json）；签名 URL（OBS 写凭证）
/// 与个人信息键整值隐藏。键名忽略大小写。
pub(crate) fn redact_http_json(v: &serde_json::Value) -> serde_json::Value {
    hide_personal(crate::textlog::redact_json(v))
}

fn hide_personal(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    const HIDDEN_KEYS: &[&str] = &[
        "signedUrl",
        "url",
        "avatar",
        "name",
        "sname",
        "realName",
        "nickname",
        "username",
        "userName",
        "alias",
        "telephone",
        "phone",
        "mobile",
        "email",
        "birthday",
        "sid",
        "studentId",
        "studentNo",
        "stuNo",
        "stuNum",
        "idCard",
        "idNo",
        "address",
        "depart",
        "gradeClass",
        "className",
        "ip",
    ];
    match v {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(key, value)| {
                    let hidden = HIDDEN_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k));
                    let value = match value {
                        Value::String(_) | Value::Number(_) | Value::Bool(_) if hidden => {
                            Value::String("****".into())
                        }
                        other => hide_personal(other),
                    };
                    (key, value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(hide_personal).collect()),
        other => other,
    }
}

/// 业务结果检查：error == 10000 → Ok(business)；否则 Err(描述)。
pub fn check_business(v: serde_json::Value) -> Result<serde_json::Value, String> {
    let err = v.get("error").and_then(|e| e.as_i64()).unwrap_or(0);
    if err == 10000 {
        normalize_business_data(v)
    } else {
        let msg = v
            .get("message")
            .or_else(|| v.get("msg"))
            .and_then(|m| m.as_str())
            .unwrap_or("(无消息)");
        Err(format!("业务错误 {err}: {msg}"))
    }
}

/// Some successful endpoints encode the business data object/array as JSON text.
/// Decode only that known wrapper, retaining ordinary text results unchanged.
pub(crate) fn normalize_business_data(
    mut v: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let decoded = match v.get("data") {
        Some(serde_json::Value::String(text)) => {
            let text = text.trim();
            if text.starts_with('{') || text.starts_with('[') {
                if text.len() > 8 * 1024 * 1024 {
                    return Err("业务 data JSON 超过 8 MiB".into());
                }
                Some(
                    serde_json::from_str(text)
                        .map_err(|_| "业务 data JSON 格式无效".to_string())?,
                )
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(data) = decoded {
        v["data"] = data;
    }
    Ok(v)
}

/// 业务字段查找：先顶层，再 data 子对象。
pub fn get_field<'a>(biz: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    if let Some(v) = biz.get(name) {
        if !v.is_null() {
            return Some(v);
        }
    }
    biz.get("data").and_then(|d| d.get(name))
}

/// data 字段如果是 JSON 字符串则解析（Obs 一层包裹语义）。
pub fn parse_data_field(biz: &serde_json::Value) -> serde_json::Value {
    match biz.get("data") {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
        }
        Some(v) => v.clone(),
        None => serde_json::Value::Null,
    }
}

pub fn ureq_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            format!("HTTP {code}: {}", crate::textlog::truncate(&body, 300))
        }
        ureq::Error::Transport(t) => format!("网络错误: {t}"),
    }
}

pub const SESSION_EXPIRED: &str = "登录已失效（HTTP 401）：请重新登录获取有效凭证";

pub fn is_session_expired(error: &str) -> bool {
    error.contains(SESSION_EXPIRED)
}

// Only authenticated school API requests may invalidate a school session.
// Public map services and unauthenticated login failures use their own errors.
fn request_error(error: ureq::Error, authenticated: bool) -> String {
    if authenticated && matches!(&error, ureq::Error::Status(401, _)) {
        SESSION_EXPIRED.into()
    } else {
        ureq_err(error)
    }
}

#[cfg(test)]
mod business_data_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn successful_json_text_data_matches_object_and_array_responses() {
        for data in [json!({"geoFences":[],"runAreaId":42}), json!([{"id":1}])] {
            let object = json!({"error":10000,"data":data});
            let encoded = json!({"error":10000,"data":data.to_string()});
            assert_eq!(
                check_business(encoded).unwrap(),
                check_business(object).unwrap()
            );
        }
        for data in ["操作成功", "https://example.invalid/file", "null", "42", ""] {
            let response = json!({"error":10000,"data":data});
            assert_eq!(check_business(response.clone()).unwrap(), response);
        }
    }

    #[test]
    fn invalid_or_excessive_json_text_is_not_silently_empty() {
        for data in [
            "{broken-private-payload".to_string(),
            format!("[\"{}\"]", "x".repeat(8 * 1024 * 1024)),
        ] {
            let error = check_business(json!({"error":10000,"data":data})).unwrap_err();
            assert!(error.contains("data JSON"));
            assert!(!error.contains("private-payload"));
        }
        assert!(check_business(json!({"error":10603,"data":"{broken"}))
            .unwrap_err()
            .starts_with("业务错误 10603"));
    }

    #[test]
    fn debug_log_hides_obs_write_url_and_personal_fields() {
        let signed = "https://bucket.example.invalid/run_data/x?Signature=abc&AccessKeyId=k";
        let value = json!({"error":10000,"data":{
            "signedUrl":signed,"URL":signed,"token":"abcdefghijkl",
            "name":"张三","sname":"李四","nickName":"neko","telephone":"13800000000",
            "sid":20240001,"email":"a@example.invalid","list":[{"userName":"u","rrid":7}],
            "uid":42,"message":"成功","totalDis":1000.0}});
        let redacted = redact_http_json(&value);
        let text = redacted.to_string();
        for secret in [
            signed,
            "abcdefghijkl",
            "张三",
            "李四",
            "neko",
            "13800000000",
            "20240001",
            "a@example.invalid",
            "\"u\"",
        ] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        assert_eq!(redacted["data"]["token"], "abcd****ijkl");
        assert_eq!(redacted["data"]["signedUrl"], "****");
        assert_eq!(redacted["data"]["uid"], 42);
        assert_eq!(redacted["data"]["list"][0]["rrid"], 7);
        assert_eq!(redacted["data"]["message"], "成功");
        assert_eq!(redacted["error"], 10000);
        assert_eq!(
            value["data"]["signedUrl"], signed,
            "original data must stay intact"
        );
    }

    #[test]
    fn body_read_errors_are_reported_not_treated_as_empty_json() {
        let ok = ureq::Response::new(200, "OK", "{\"error\":10000}").unwrap();
        assert_eq!(read_body(ok).unwrap(), "{\"error\":10000}");
        // ureq into_string 有 10 MiB 上限：超限必须报读取失败，而不是空串。
        let huge = "x".repeat(10 * 1024 * 1024 + 1);
        let error = read_body(ureq::Response::new(200, "OK", &huge).unwrap()).unwrap_err();
        assert!(error.starts_with("读取响应失败"), "{error}");
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;
    fn response_error(code: u16, body: &str) -> ureq::Error {
        ureq::Error::Status(code, ureq::Response::new(code, "fixture", body).unwrap())
    }
    #[test]
    fn authenticated_401_requires_login_without_exposing_raw_response() {
        let error = request_error(
            response_error(
                401,
                r#"{"message":"token有误或缓存中token不存在","token":"secret-fixture"}"#,
            ),
            true,
        );
        assert_eq!(error, SESSION_EXPIRED);
        assert!(is_session_expired(&error));
        assert!(!error.contains("secret-fixture"));
    }
    #[test]
    fn login_and_non_auth_http_failures_do_not_expire_a_session() {
        assert!(!is_session_expired(&request_error(
            response_error(401, "login rejected"),
            false
        )));
        for code in [403, 429, 500] {
            assert!(!is_session_expired(&request_error(
                response_error(code, "fixture"),
                true
            )));
        }
    }
    #[test]
    fn chinese_http_errors_are_truncated_on_character_boundaries() {
        let body = format!("a{}", "错误".repeat(100));
        let error = ureq_err(response_error(500, &body));
        assert!(error.starts_with("HTTP 500: a错误"));
    }
}

#[cfg(test)]
mod shared_auth_tests {
    use super::*;
    #[test]
    fn one_rejection_stops_other_school_reads_and_only_notifies_once() {
        let (tx, rx) = std::sync::mpsc::channel();
        let context = AuthContext {
            epoch: 7,
            rejected: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            notify: tx,
        };
        let session = Session {
            uid: 42,
            token: "test-token".into(),
            ..Default::default()
        };
        let mut first = ApiClient::new(HeaderIdentity::default(), Some(session.clone()))
            .with_auth_context(Some(context.clone()));
        // Exercise the actual request/error path using an isolated loopback HTTP server.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "mock HTTP request did not arrive"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut chunk = [0u8; 4096];
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
                if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
            assert!(headers.contains("headersign:"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            while request.len() < header_end + length {
                let mut chunk = [0u8; 4096];
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
            }
            let body = r#"{"status":401,"message":"token有误或缓存中token不存在"}"#;
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        assert_eq!(
            first
                .call_host(&host, "POST", crate::api::fence::FENCE_PATH, "{}", &[])
                .unwrap_err(),
            SESSION_EXPIRED
        );
        server.join().unwrap();
        let failure = rx.try_recv().unwrap();
        assert_eq!(failure.epoch, 7);
        assert_eq!(failure.uid, 42);
        for path in [
            crate::api::fence::FENCE_PATH,
            crate::api::cheat::CHEATLIST_PATH,
            "/api/v70230/runnings/records",
            crate::api::user::HOME_PAGE_INFO_PATH,
        ] {
            let mut client = ApiClient::new(HeaderIdentity::default(), Some(session.clone()))
                .with_auth_context(Some(context.clone()));
            // Rejection must happen before contacting this unreachable loopback address.
            assert_eq!(
                client
                    .call_host("http://127.0.0.1:1", "POST", path, "{}", &[])
                    .unwrap_err(),
                SESSION_EXPIRED
            );
        }
        assert!(rx.try_recv().is_err());
    }
}
