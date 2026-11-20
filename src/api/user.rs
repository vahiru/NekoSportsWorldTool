//! 个人信息。
//! 聚合：登录 profile / 主页信息（legacy: v41 HomePageInfo；current: v70300+v70380）
//! / v41/running/getPersonalSemesterInfo(runMode) / v55 recordssummary
//! / v44 personalSemesterCompleted(需 rrid)。

#[cfg(any(feature = "gui", feature = "flutter"))]
use super::client::{parse_data_field, ApiClient};
#[cfg(any(feature = "gui", feature = "flutter"))]
use super::model::ApiMode;
#[cfg(any(feature = "gui", feature = "flutter"))]
use serde_json::json;
#[cfg(any(feature = "gui", feature = "flutter", test))]
use serde_json::Value;

#[cfg(any(feature = "gui", feature = "flutter", test))]
pub const HOME_PAGE_INFO_PATH: &str = "/api/v41/user/HomePageInfo";
#[cfg(any(feature = "gui", feature = "flutter", test))]
pub const HOME_PAGE_BASE_PATH: &str = "/api/v70300/user/homePageBaseUserInfo";
#[cfg(any(feature = "gui", feature = "flutter", test))]
pub const HOME_PAGE_EXTENDED_PATH: &str = "/api/v70380/user/homePageExtendedInfo";
#[cfg(any(feature = "gui", feature = "flutter"))]
pub const PERSONAL_SEMESTER_PATH: &str = "/api/v41/running/getPersonalSemesterInfo";
pub const SEMESTER_COMPLETED_PATH: &str = "/api/v44/runnings/personalSemesterCompleted";

#[cfg(any(feature = "gui", feature = "flutter", test))]
pub struct MyInfo {
    pub errors: Vec<String>,
    // 只由 GUI 的“我的”页读取。
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub profile: Value,
    pub home_page: Value,
    pub personal_semester: Value,
    pub summary: Value,
    pub completed: Value,
}

#[cfg(any(feature = "gui", feature = "flutter"))]
fn fetch_current_home(client: &mut ApiClient, errors: &mut Vec<String>) -> Value {
    let Some(uid) = client.login.as_ref().map(|s| s.uid) else {
        errors.push("主页：未登录，缺少 uid".into());
        return Value::Null;
    };
    let mut base = Value::Null;
    let mut extended = Value::Null;
    for (label, path, slot) in [
        ("主页基础", HOME_PAGE_BASE_PATH, &mut base),
        ("主页扩展", HOME_PAGE_EXTENDED_PATH, &mut extended),
    ] {
        let url = format!("{path}?uid={uid}");
        match client.call_host(super::model::DISCOVERY, "GET", &url, "{}", &[]) {
            Ok(biz) => *slot = parse_data_field(&biz),
            Err(e) => errors.push(format!("{label}：{e}")),
        }
    }
    merge_home_page(base, extended)
}

#[cfg(any(feature = "gui", feature = "flutter", test))]
fn merge_home_page(base: Value, extended: Value) -> Value {
    let mut merged = serde_json::Map::new();
    let mut any = false;
    for value in [base, extended] {
        if let Value::Object(map) = value {
            merged.extend(map);
            any = true;
        }
    }
    if any {
        Value::Object(merged)
    } else {
        Value::Null
    }
}

#[cfg(any(feature = "gui", feature = "flutter"))]
/// 拉取我的页全部数据；单项失败不阻塞其他项。
pub fn fetch_my_info(client: &mut ApiClient, log: &mut dyn FnMut(&str)) -> MyInfo {
    let mut errors = Vec::new();
    let profile = client
        .login
        .as_ref()
        .map(|s| s.profile.clone())
        .unwrap_or(Value::Null);

    let home_page = match client.api_mode() {
        ApiMode::Legacy => client
            .call("GET", HOME_PAGE_INFO_PATH, "{}", &[])
            .map(|biz| parse_data_field(&biz))
            .unwrap_or_else(|e| {
                errors.push(format!("主页：{e}"));
                Value::Null
            }),
        ApiMode::Current => fetch_current_home(client, &mut errors),
    };
    if home_page.is_null() {
        log("⚠ [user] 主页数据拉取失败");
    }

    // 文档确认：getPersonalSemesterInfo 需要传 runMode
    let personal_semester = client
        .call(
            "POST",
            PERSONAL_SEMESTER_PATH,
            &json!({ "runMode": 1 }).to_string(),
            &[],
        )
        .map(|biz| parse_data_field(&biz))
        .unwrap_or_else(|e| {
            errors.push(e);
            Value::Null
        });

    let summary = client
        .call(
            "POST",
            super::semester::SUMMARY_PATH,
            &json!({}).to_string(),
            &[],
        )
        .map(|biz| parse_data_field(&biz))
        .unwrap_or_else(|e| {
            errors.push(e);
            Value::Null
        });

    // 学期完成状态需要最近一条记录的 rrid
    let completed = match super::records::fetch_records(client) {
        Ok(rows) if !rows.is_empty() => client
            .call(
                "POST",
                SEMESTER_COMPLETED_PATH,
                &json!({ "rrid": rows[0].rrid }).to_string(),
                &[],
            )
            .map(|biz| parse_data_field(&biz))
            .unwrap_or_else(|e| {
                errors.push(format!("完成状态：{e}"));
                Value::Null
            }),
        Err(e) => {
            errors.push(format!("最近记录：{e}"));
            Value::Null
        }
        _ => Value::Null,
    };

    MyInfo {
        errors,
        profile,
        home_page,
        personal_semester,
        summary,
        completed,
    }
}

#[cfg(any(feature = "gui", test))]
impl MyInfo {
    pub fn refresh_message(&self) -> String {
        if !self.errors.is_empty() {
            format!(
                "⚠ [user] 个人数据刷新未全部成功：{}",
                self.errors.join("；")
            )
        } else if [
            &self.home_page,
            &self.personal_semester,
            &self.summary,
            &self.completed,
        ]
        .iter()
        .all(|v| v.is_null())
        {
            "[user] 未获取到新的个人数据；登录资料仅为缓存".into()
        } else {
            "√ [user] 我的页数据已更新".into()
        }
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cached_profile_and_failed_reads_cannot_claim_a_successful_refresh() {
        let mut info = MyInfo {
            errors: vec![],
            profile: json!({"name":"cached"}),
            home_page: Value::Null,
            personal_semester: Value::Null,
            summary: Value::Null,
            completed: Value::Null,
        };
        assert!(!info.refresh_message().contains("已更新"));
        info.home_page = json!({"count":1});
        info.errors.push("请求失败".into());
        assert!(!info.refresh_message().contains("已更新"));
        info.errors.clear();
        assert!(info.refresh_message().contains("已更新"));
    }

    #[test]
    fn current_home_merges_base_and_extended_with_extended_winning() {
        let merged = merge_home_page(
            json!({"name":"base","fansNum":1}),
            json!({"fansNum":2,"point":3}),
        );
        assert_eq!(merged["name"], "base");
        assert_eq!(merged["fansNum"], 2);
        assert_eq!(merged["point"], 3);
        assert!(merge_home_page(Value::Null, Value::Null).is_null());
    }
}
