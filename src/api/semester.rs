//! 学期完成度。
//! 汇总：POST /api/v55/runnings/recordssummary/semester {}；
//! 个人完成度：POST /api/v41/running/getPersonalSemesterInfo {}（结构按实际响应落地）。

use super::client::ApiClient;
use serde_json::{json, Value};

pub const SUMMARY_PATH: &str = "/api/v55/runnings/recordssummary/semester";
pub const PERSONAL_PATH: &str = "/api/v41/running/getPersonalSemesterInfo";

/// 学期汇总（B1 实测字段）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SemesterSummary {
    pub sname: String,
    pub semester_dis: f64,
    pub semester_valid_dis: f64,
    pub semester_count: i64,
    pub semester_valid_count: i64,
    #[serde(default)]
    pub completed_proportion: Option<Value>,
}

#[allow(dead_code)]
pub struct SemesterResult {
    pub summary: Option<SemesterSummary>,
    /// getPersonalSemesterInfo 原始 data（结构未定型，全量落地供核对）。
    pub personal_raw: Value,
}

pub fn query(client: &mut ApiClient, log: &mut dyn FnMut(&str)) -> Result<SemesterResult, String> {
    // 汇总
    let biz = client.call("POST", SUMMARY_PATH, "{}", &[])?;
    let data = super::client::parse_data_field(&biz);

    // 个人完成度：学期起止 / 考核方式 / 有效次数
    let personal_raw = match client.call(
        "POST",
        PERSONAL_PATH,
        &json!({ "runMode": 1 }).to_string(),
        &[],
    ) {
        Ok(biz) => super::client::parse_data_field(&biz),
        Err(e) => {
            log(&format!("[semester] 个人完成度接口失败（不影响汇总）: {e}"));
            Value::Null
        }
    };
    // Only explicitly named fields may fill a missing summary value. Zero is
    // a valid result; semesterValid and semesterGoalCount have other meanings.
    let mut completed = Value::Null;
    if ["semesterCount", "semesterValidCount"].iter().any(|key| {
        [&data, &personal_raw]
            .iter()
            .all(|v| count(v, key).is_none())
    }) || [&data, &personal_raw]
        .iter()
        .all(|v| proportion(v).is_none())
    {
        let rrid = super::records::fetch_records(client)
            .ok()
            .and_then(|rows| rows.first().map(|r| r.rrid));
        if let Some(rrid) = rrid {
            let body = json!({ "rrid": rrid }).to_string();
            if let Ok(biz) = client.call("POST", super::user::SEMESTER_COMPLETED_PATH, &body, &[]) {
                completed = super::client::parse_data_field(&biz);
            }
        }
    }

    Ok(SemesterResult {
        summary: Some(from_sources(&[&data, &personal_raw, &completed])),
        personal_raw,
    })
}

fn count(value: &Value, key: &str) -> Option<i64> {
    value.get(key)?.as_i64().filter(|n| *n >= 0)
}

fn distance(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)?
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
}

fn proportion(value: &Value) -> Option<Value> {
    value
        .get("completedProportion")
        .filter(|v| v.is_number() || v.as_str().is_some_and(|s| !s.trim().is_empty()))
        .cloned()
}

fn from_sources(sources: &[&Value]) -> SemesterSummary {
    SemesterSummary {
        sname: sources
            .iter()
            .find_map(|v| v.get("sname")?.as_str().filter(|s| !s.is_empty()))
            .unwrap_or("")
            .into(),
        semester_dis: sources
            .iter()
            .find_map(|v| distance(v, "semesterDis"))
            .unwrap_or(0.0),
        semester_valid_dis: sources
            .iter()
            .find_map(|v| distance(v, "semesterValidDis"))
            .unwrap_or(0.0),
        semester_count: sources
            .iter()
            .find_map(|v| count(v, "semesterCount"))
            .unwrap_or(0),
        semester_valid_count: sources
            .iter()
            .find_map(|v| count(v, "semesterValidCount"))
            .unwrap_or(0),
        completed_proportion: sources.iter().find_map(|v| proportion(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_zero_wins_and_distances_and_goals_never_become_counts() {
        let first = json!({"semesterCount":0,"semesterValidCount":0,"semesterValidDis":0});
        let fallback = json!({"semesterCount":12,"semesterValidCount":10,"semesterValidDis":32000,"completedProportion":"40%"});
        let s = from_sources(&[&first, &fallback]);
        assert_eq!(
            (
                s.semester_count,
                s.semester_valid_count,
                s.semester_valid_dis
            ),
            (0, 0, 0.0)
        );
        assert_eq!(s.completed_proportion, Some(json!("40%")));
        let ambiguous = json!({"semesterValid":32000,"semesterGoalCount":60});
        let s = from_sources(&[&ambiguous]);
        assert_eq!((s.semester_count, s.semester_valid_count), (0, 0));
        let s = from_sources(&[&ambiguous, &fallback]);
        assert_eq!(
            (
                s.semester_count,
                s.semester_valid_count,
                s.semester_valid_dis
            ),
            (12, 10, 32000.0)
        );
    }
}
