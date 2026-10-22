//! OBS 上传：POST /api/obs/temporary/url 换签名 URL → PUT JSON。

use super::client::{get_field, parse_data_field, ApiClient};
use serde_json::{json, Value};

pub const OBS_SIGN_PATH: &str = "/api/obs/temporary/url";

/// 换取签名 URL。
pub fn sign_url(client: &mut ApiClient, method: &str, key: &str) -> Result<String, String> {
    let body = json!({
        "bucketName": "iydsj-hbase-hot",
        "objectKey": key,
        "method": method,
        "contentType": "application/json",
    })
    .to_string();
    let biz = client.call("POST", OBS_SIGN_PATH, &body, &[])?;
    // business.data 可能是 JSON 字符串 {"signedUrl": ...}
    let data = parse_data_field(&biz);
    let signed = data
        .get("signedUrl")
        .and_then(|v| v.as_str())
        .or_else(|| get_field(&biz, "signedUrl").and_then(|v| v.as_str()))
        .ok_or("OBS 签名响应缺 signedUrl")?;
    Ok(signed.to_string())
}

/// PUT 上传 OBS 对象（复用调用方 HTTP agent 的连接池）。
pub fn put_object(
    agent: &ureq::Agent,
    signed_url: &str,
    payload: &[u8],
    log: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let resp = agent
        .put(signed_url)
        .set("Content-Type", "application/json")
        .send_bytes(payload)
        .map_err(super::client::ureq_err)?;
    log(&format!(
        "[obs] PUT {} -> {}",
        shorten(signed_url),
        resp.status()
    ));
    Ok(())
}

/// 上传到两个 objectKey（详情页 + 兜底路径）。
pub fn upload_both_keys(
    client: &mut ApiClient,
    keys: &[String],
    payload: &[u8],
    log: &mut dyn FnMut(&str),
) -> usize {
    let mut ok = 0;
    for key in keys {
        match sign_url(client, "Put", key)
            .and_then(|url| put_object(&client.agent, &url, payload, log))
        {
            Ok(()) => ok += 1,
            Err(e) => log(&format!("[obs] PUT {key} 失败: {e}")),
        }
    }
    ok
}

fn shorten(url: &str) -> String {
    // 只显示 objectKey 部分：去掉查询串（签名凭证），按字符截断避免劈开多字节字符
    let path = url.split('?').next().unwrap_or_default();
    let start = path.find("run_data").unwrap_or(0);
    crate::textlog::truncate(&path[start..], 60)
}

/// 回读验证（GET 签名）。
#[allow(dead_code)]
pub fn fetch_object(
    client: &mut ApiClient,
    key: &str,
    log: &mut dyn FnMut(&str),
) -> Result<Value, String> {
    let signed = sign_url(client, "Get", key)?;
    let resp = client
        .agent
        .get(&signed)
        .call()
        .map_err(super::client::ureq_err)?;
    let text = super::client::read_body(resp)?;
    log(&format!("[obs] GET 回读 {} 字节", text.len()));
    serde_json::from_str(&text).map_err(|e| format!("OBS 对象解析失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_is_char_safe_and_drops_signature_query() {
        let url = format!(
            "https://obs.example.invalid/run_data/{}/1.json?AccessKeyId=k&Signature=secret",
            "跑步数据".repeat(30)
        );
        let short = shorten(&url);
        assert!(short.starts_with("run_data/跑步"));
        assert_eq!(short.chars().count(), 60);
        assert!(!short.contains("Signature"));
        // 起点后不足 60 字节、无 run_data、空串均不得 panic
        for url in [
            "",
            "https://x.invalid/a?Signature=s",
            "run_data/中",
            "中文run_data/文",
        ] {
            assert!(!shorten(url).contains("Signature"));
        }
    }
}
