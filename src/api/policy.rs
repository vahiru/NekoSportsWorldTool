//! 跑步策略：POST /api/v70103/runModePolicy。
//! 返回 data.runRuleModel.minDistance（点位请求距离和提交 selDistance，单位米）与 data.policy。

use super::client::{get_field, ApiClient};
use super::fence::point_xy;
use serde_json::{json, Value};

pub const POLICY_PATH: &str = "/api/v70103/runModePolicy";

pub struct PolicyInfo {
    pub timestamp: i64,
    pub policy: i64,
    pub min_distance: i64,
    pub points_distance: super::points::PointsDistance,
    pub valid_time: i64,
    /// 7.4.0 RunRuleModel.minRunTime（秒）；0 = 未下发，按旧口径回退 totalTime。
    pub min_run_time: i64,
    /// 必经点（BD 系，与打卡点同系），policy 响应里若有则返回。
    pub must_points: Vec<(f64, f64)>,
    pub run_areas: Vec<super::campus::RunArea>,
    pub geo_fence: Option<super::fence::GeoFenceResponse>,
}

/// 从 policy 响应 `data` 中防御式提取必经点列表（字段名不确定，逐个尝试）。
fn extract_must_points(v: &serde_json::Value) -> Result<Vec<(f64, f64)>, String> {
    let mut points = Vec::new();
    for name in [
        "pointList",
        "runPointList",
        "mustPointList",
        "passPointList",
        "mustPoints",
        "passPoints",
        "runPoints",
        "points",
        "nodeList",
        "checkPointList",
    ] {
        let Some(value) = get_field(v, name).filter(|v| !v.is_null()) else {
            continue;
        };
        let arr = value
            .as_array()
            .ok_or_else(|| format!("策略点位 {name} 不是列表"))?;
        for (index, value) in arr.iter().enumerate() {
            let point =
                point_xy(value).ok_or_else(|| format!("策略点位 {name}[{index}] 坐标无效"))?;
            if !points
                .iter()
                .any(|p: &(f64, f64)| (p.0 - point.0).abs() < 1e-6 && (p.1 - point.1).abs() < 1e-6)
            {
                points.push(point);
            }
        }
    }
    Ok(points)
}

/// body：{"runMode":1,"ruleUpdateTime":0,"geoFenceUpdateTime":0,"selectUnid":<unid>,"operateType":0}
pub fn fetch_policy(client: &mut ApiClient) -> Result<PolicyInfo, String> {
    let unid = client
        .login
        .as_ref()
        .map(|s| s.unid.clone())
        .unwrap_or_default();
    let select_unid = unid.parse::<i64>().unwrap_or(0);
    let body = json!({
        "runMode": 1,
        "ruleUpdateTime": 0,
        "geoFenceUpdateTime": 0,
        "selectUnid": select_unid,
        "operateType": 0,
    })
    .to_string();
    let biz = client.call("POST", POLICY_PATH, &body, &[])?;
    parse_response(&biz)
}

pub(crate) fn parse_response(biz: &Value) -> Result<PolicyInfo, String> {
    let biz = super::client::normalize_business_data(biz.clone())?;
    let timestamp = get_field(&biz, "timestamp")
        .and_then(|t| t.as_i64())
        .ok_or("policy 响应缺 timestamp")?;
    let policy = get_field(&biz, "policy")
        .and_then(|t| t.as_i64())
        .unwrap_or(0);
    let legacy = get_field(&biz, "runRuleModel")
        .or_else(|| biz.get("runRuleModel"))
        .is_some_and(Value::is_null);
    let rule = get_field(&biz, "runRuleModel")
        .cloned()
        .unwrap_or(Value::Null);
    let min_distance = if legacy {
        1000
    } else {
        rule.get("minDistance")
            .and_then(Value::as_i64)
            .ok_or("学校策略缺少有效的 runRuleModel.minDistance，请刷新学校策略")?
    };
    super::points::points_path(min_distance)?;
    Ok(PolicyInfo {
        timestamp,
        policy,
        min_distance,
        points_distance: if legacy {
            super::points::PointsDistance::Legacy
        } else {
            super::points::PointsDistance::School(min_distance)
        },
        valid_time: rule.get("validTime").and_then(|t| t.as_i64()).unwrap_or(0),
        // 7.4.0 新字段；缺失时保持 0，不影响 7.3.x 学校。
        min_run_time: rule
            .get("minRunTime")
            .and_then(|t| t.as_i64())
            .unwrap_or(0)
            .max(0),
        must_points: extract_must_points(&biz)?,
        run_areas: super::campus::parse_run_areas(get_field(&biz, "runAreaModels"))?,
        geo_fence: get_field(&biz, "geoFence")
            .filter(|v| !v.is_null())
            .map(|v| super::fence::parse_response(v.clone()))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_null_rule_uses_legacy_contract_only() {
        let data = json!({"policy":0,"timestamp":0,"runRuleModel":null,"geoFence":null,
            "message":null,"runAreaModels":null,"freezeRunTime":29363,"runFaceCheckConfigDTO":null});
        for response in [
            data.clone(),
            json!({"error":10000,"data":data.clone()}),
            json!({"data":data.to_string()}),
        ] {
            let policy = parse_response(&response).unwrap();
            assert_eq!(policy.min_distance, 1000);
            assert_eq!(
                policy.points_distance,
                super::super::points::PointsDistance::Legacy
            );
            assert_eq!(policy.timestamp, 0);
            assert!(policy.run_areas.is_empty());
        }
        assert!(parse_response(&json!({"timestamp":0})).is_err());
        let policy =
            parse_response(&json!({"timestamp":1,"runRuleModel":{"minDistance":1000}})).unwrap();
        assert_eq!(
            policy.points_distance,
            super::super::points::PointsDistance::School(1000)
        );
    }

    #[test]
    fn checkpoint_distance_requires_explicit_positive_integer_metres() {
        for rule in [
            json!({}),
            json!({"minDistance":null}),
            json!({"minDistance":0}),
            json!({"minDistance":-1}),
            json!({"minDistance":2.5}),
            json!({"minDistance":"2000"}),
            json!({"minDistance":2147483648_i64}),
        ] {
            assert!(parse_response(&json!({"timestamp":1,"runRuleModel":rule})).is_err());
        }
        assert!(parse_response(&json!({"timestamp":1})).is_err());
        assert_eq!(
            parse_response(&json!({"timestamp":1,"runRuleModel":{"minDistance":2000}}))
                .unwrap()
                .min_distance,
            2000
        );
    }
    #[test]
    fn policy_geo_fence_is_validated_including_json_encoded_data() {
        let data = json!({"timestamp":1,"runRuleModel":{"minDistance":1000},"runAreaModels":[{"id":73,"name":"测试跑区","valid":true,"fenceIds":[701]}],"geoFence":{"geoFences":[]}});
        let p = parse_response(&json!({"data":data.to_string()})).unwrap();
        assert_eq!(p.run_areas[0].id, 73);
        assert!(p.geo_fence.unwrap().polygons.is_empty());
        for fence in [
            json!({}),
            json!("bad"),
            json!({"geoFences":[{"points":[]} ]}),
        ] {
            assert!(parse_response(
                &json!({"timestamp":1,"runRuleModel":{"minDistance":1000},"geoFence":fence})
            )
            .is_err());
        }
    }
    #[test]
    fn json_text_policy_preserves_required_points_and_rules() {
        let data = json!({"timestamp":123,"policy":4,"runRuleModel":{"minDistance":2000,"validTime":600,"minRunTime":420},"mustPoints":[{"lat":"39.9","lon":"116.4"}]});
        for response in [json!({"data":data}), json!({"data":data.to_string()})] {
            let policy = parse_response(&response).unwrap();
            assert_eq!(policy.timestamp, 123);
            assert_eq!(policy.policy, 4);
            assert_eq!(policy.min_distance, 2000);
            assert_eq!(policy.valid_time, 600);
            assert_eq!(policy.min_run_time, 420);
            assert_eq!(policy.must_points, vec![(39.9, 116.4)]);
        }
        // 7.3.x 学校未下发 minRunTime → 0（提交回退 totalTime）。
        let legacy =
            parse_response(&json!({"timestamp":1,"runRuleModel":{"minDistance":1000}})).unwrap();
        assert_eq!(legacy.min_run_time, 0);
        // 异常负值按未下发处理。
        let negative = parse_response(
            &json!({"timestamp":1,"runRuleModel":{"minDistance":1000,"minRunTime":-5}}),
        )
        .unwrap();
        assert_eq!(negative.min_run_time, 0);
        assert!(parse_response(&json!({"data":"{broken"})).is_err());
    }

    #[test]
    fn merges_all_recognized_lists_without_losing_first_point() {
        let a = json!({"lat":39.9,"lon":116.4});
        let b = json!({"lat":39.91,"lon":116.41});
        let v = json!({"data":{"pointList":[a.clone()],"mustPoints":[a,b]}});
        assert_eq!(
            extract_must_points(&v).unwrap(),
            vec![(39.9, 116.4), (39.91, 116.41)]
        );
        assert!(extract_must_points(&json!({"mustPoints":null}))
            .unwrap()
            .is_empty());
    }
    #[test]
    fn malformed_required_points_are_errors_not_omissions() {
        assert!(
            extract_must_points(&json!({"mustPoints":[{}, {"lat":39.9,"lon":116.4}]})).is_err()
        );
        assert!(extract_must_points(&json!({"mustPoints":"bad"})).is_err());
    }
}
