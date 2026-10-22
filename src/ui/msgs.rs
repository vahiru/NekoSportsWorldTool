//! 消息分发（类型化事件）与弹窗构建。

use super::about::FinishAction;
use super::{App, PopupInfo, UiEvent};
use crate::api::model;
use chrono::TimeZone;

impl App {
    fn expire_road_session(&mut self) {
        self.retire_session_jobs();
        self.auth_guard
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.data_page.cheat = None;
        self.data_page.cheat_error = Some("登录已失效，请重新登录后检查".into());
        self.data_page.semester = None;
        self.user_page.set_info(None);
        self.session = None;
        model::clear_session();
        self.auth_expired = true;
        self.osm_page.cancel.cancel();
        self.osm_page.request_id = self.osm_page.request_id.wrapping_add(1);
        self.osm_page.busy = false;
        self.run_page.preview_cancel.cancel();
        self.run_page.preview_inputs = None;
        self.run_page.preview = None;
        self.run_page.preview_stale = true;
        self.run_page.check = None;
        self.status = crate::api::client::SESSION_EXPIRED.into();
    }

    fn fetch_failed(&mut self, label: &str, error: &str) {
        self.status = format!("{label}刷新失败：{error}");
    }

    pub(crate) fn poll_messages(&mut self) {
        self.poll_preview_results();
        while let Ok(msg) = self.rx.try_recv() {
            // 旧会话（登出 / 重新登录 / 失效前）发起的任务结果一律丢弃
            if msg.epoch.is_some_and(|epoch| epoch != self.session_epoch) {
                continue;
            }
            self.handle_event(msg.event);
        }
        self.poll_network_loads();
        while let Ok(failure) = self.auth_rx.try_recv() {
            if failure.epoch == self.session_epoch
                && self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.uid == failure.uid && s.token == failure.token)
            {
                self.expire_road_session();
            }
        }
        if self.auth_expired
            && !self.login_busy
            && !self.status.starts_with("登录失败")
            && !self.status.starts_with("请填写")
        {
            self.status = crate::api::client::SESSION_EXPIRED.into();
        }
    }

    fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Log(line) => self.log.push(&line),
            UiEvent::Ip(ip) => self.ip = ip,
            UiEvent::RunStage(stage) => self.status = stage,
            UiEvent::RoadStage { req, stage } => {
                if self.osm_page.busy && req == self.osm_page.request_id {
                    self.osm_page.status = stage;
                }
            }
            UiEvent::Login(result) => self.on_login(result),
            UiEvent::RunDone(v) => self.on_run_done(&v),
            UiEvent::AiDone(v) => self.on_ai_done(&v),
            UiEvent::AiBatchProgress { batch, done, total } => {
                if let Some(state) = self.ai_batch.as_mut().filter(|b| b.id == batch) {
                    state.done = done;
                    state.total = total;
                }
            }
            UiEvent::AiList { req, result } => {
                if req != self.reqs.ai_list {
                    return;
                }
                self.ai_list_busy = false;
                match result {
                    Ok(list) => {
                        self.status = format!("已拉取 {} 个 AI 项目", list.len());
                        self.ai_page.set_list(list);
                        self.ai_page.selected = 0;
                    }
                    Err(e) => self.fetch_failed("AI 项目", &e),
                }
            }
            UiEvent::Records { req, result } => {
                if req != self.reqs.records {
                    return;
                }
                self.records_busy = false;
                match result {
                    Ok(rows) => {
                        self.status = format!("已刷新 {} 条记录", rows.len());
                        self.records_page.set_rows(rows);
                    }
                    Err(e) => self.fetch_failed("跑步记录", &e),
                }
            }
            UiEvent::AiRecords { req, result } => {
                if req != self.reqs.ai_records {
                    return;
                }
                self.ai_records_busy = false;
                match result {
                    Ok((groups, total)) => self.records_page.set_ai_groups(groups, total),
                    Err(e) => self.fetch_failed("AI 记录", &e),
                }
            }
            UiEvent::RunDetail(raw) => {
                // 只接受当前打开记录的回包：迟到的旧回包不能覆盖新记录
                let current = self.records_page.detail_rrid;
                if current.is_some() && raw.get("rrid").and_then(|v| v.as_i64()) == current {
                    self.records_page.detail_raw = Some(raw);
                    self.records_page.detail_loading = false;
                }
            }
            UiEvent::AiDetail { id, raw } => {
                if self.records_page.ai_detail_id == Some(id) {
                    self.records_page.ai_detail_raw = Some(raw);
                    self.records_page.ai_detail_loading = false;
                }
            }
            UiEvent::Semester { req, result } => {
                if req != self.reqs.semester {
                    return;
                }
                self.semester_busy = false;
                match result {
                    Ok(Some(summary)) => self.data_page.semester = Some(summary),
                    Ok(None) => self.status = "学期数据为空".into(),
                    Err(e) => {
                        self.data_page.semester = None;
                        self.fetch_failed("学期", &e);
                    }
                }
            }
            UiEvent::Cheat { req, result } => {
                if req != self.reqs.cheat {
                    return;
                }
                self.data_page.cheat_checking = false;
                match result {
                    Ok(report) => {
                        self.data_page.cheat = Some(report);
                        self.data_page.cheat_error = None;
                    }
                    Err(e) => {
                        self.data_page.cheat = None;
                        self.data_page.cheat_error = Some(e.clone());
                        self.fetch_failed("违规检查", &e);
                    }
                }
            }
            UiEvent::Rank { req, result } => {
                if req != self.reqs.rank {
                    return;
                }
                self.rank_busy = false;
                match result {
                    Ok(rows) => self.data_page.rank_rows = rows,
                    Err(e) => self.fetch_failed("排行榜", &e),
                }
            }
            UiEvent::User { req, info } => {
                if req != self.reqs.user {
                    return;
                }
                self.user_busy = false;
                self.user_page.set_info(Some(info));
            }
            UiEvent::UpdateCheck(result) => self.on_update_check(result),
            UiEvent::UpdateProgress { done, total } => {
                self.update.done = done;
                self.update.total = total;
            }
            UiEvent::UpdateDone(result) => self.on_update_done(result),
            UiEvent::CacheEntries { req, entries } => {
                if req == self.reqs.cache_entries {
                    self.osm_page.cache_entries = entries;
                    self.osm_page.entries_loading = false;
                }
            }
            UiEvent::Gpx { req, outcome } => self.on_gpx(req, outcome),
        }
    }

    fn on_login(&mut self, result: Result<model::Session, String>) {
        self.login_busy = false;
        match result {
            Ok(session) => {
                self.auth_expired = false;
                self.status = format!("已登录：{}", session.name);
                self.session = Some(session);
                // 登录成功后补齐全套数据（首次登录时项目列表等均为空）
                self.refresh_data_page();
                self.refresh_records();
                self.refresh_user_page();
                self.refresh_ai_list();
                self.refresh_points();
            }
            Err(message) => self.status = format!("登录失败：{message}"),
        }
    }

    fn on_run_done(&mut self, val: &serde_json::Value) {
        self.run_busy = false;
        if val.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
            self.status = format!(
                "跑步提交成功 rrid={}（OBS {}/2）",
                val["rrid"].as_i64().unwrap_or(0),
                val["obs_ok"].as_i64().unwrap_or(0)
            );
            self.popup = Some(run_popup(val));
            self.refresh_data_page();
            self.run_page.preview_stale = true;
        } else {
            self.status = format!("跑步操作未完成：{}", val["message"].as_str().unwrap_or(""));
        }
    }

    fn on_ai_done(&mut self, val: &serde_json::Value) {
        if let Some(batch) = val.get("batch_id").and_then(|v| v.as_u64()) {
            if self.ai_batch.as_ref().map(|b| b.id) != Some(batch) {
                return;
            }
            self.ai_batch = None;
        }
        self.ai_busy = false;
        if val.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
            self.status = match val.get("stopped").and_then(|v| v.as_str()) {
                Some(reason) => format!("AI 批量补签已中止：{reason}"),
                None => "AI 运动提交成功".into(),
            };
            self.popup = Some(ai_popup(val));
        } else {
            self.status = format!("AI 提交失败：{}", val["message"].as_str().unwrap_or(""));
        }
    }

    fn on_update_check(&mut self, result: Result<Option<crate::update::ReleaseInfo>, String>) {
        self.update.checking = false;
        match result {
            Ok(Some(rel)) => {
                self.status = format!("发现新版本 {}", rel.tag);
                self.update.latest = Some(rel.clone());
                self.update.up_to_date = false;
                self.update.confirm = Some(rel);
            }
            Ok(None) => {
                self.update.up_to_date = true;
                self.status = "已是最新版本".into();
            }
            Err(msg) => {
                self.status = format!("检查更新失败：{msg}");
                self.update.check_error = Some(msg);
            }
        }
    }

    fn on_update_done(&mut self, result: Result<String, String>) {
        self.update.downloading = false;
        match result {
            Ok(tag) => {
                self.status = format!("√ 已更新到 {tag}");
                #[cfg(target_os = "android")]
                {
                    self.update.finish = Some(FinishAction::InstallApk { tag });
                }
                #[cfg(not(target_os = "android"))]
                {
                    self.update.finish = Some(FinishAction::Restart { tag });
                }
            }
            Err(msg) => {
                self.update.check_error = Some(msg.clone());
                self.status = format!("× 更新失败：{msg}");
                self.popup = Some(PopupInfo {
                    title: "更新失败".into(),
                    lines: vec![msg],
                });
            }
        }
    }

    /// OSM / 学校路网加载回传。
    fn poll_network_loads(&mut self) {
        while let Ok(res) = self.net_rx.try_recv() {
            if res.request_id != self.osm_page.request_id {
                continue;
            }
            if let Some((uid, unid, anchor, token)) = &res.scope {
                // A delayed failure from an old token cannot invalidate a fresh login.
                if self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.uid == *uid && s.token != *token)
                {
                    continue;
                }
                if !self
                    .session
                    .as_ref()
                    .map(|s| s.uid == *uid && s.unid == *unid)
                    .unwrap_or(false)
                    || self.identity.anchor_coordinate().ok() != *anchor
                {
                    self.osm_page.busy = false;
                    self.network = None;
                    self.run_page.preview = None;
                    self.run_page.preview_inputs = None;
                    self.osm_page.status = "学校或定位已变化，请重新获取路网".into();
                    continue;
                }
            }
            let refresh_manual_metadata = !res.pending
                && !res.offline
                && res.scope.is_none()
                && res.inputs.is_none()
                && res.result.is_ok()
                && !self.config.osm_path.is_empty()
                && self
                    .session
                    .as_ref()
                    .is_some_and(model::Session::is_logged_in)
                && self.run_page.route_mode == crate::track::generate_road::RouteMode::Road;
            self.osm_page.busy = res.pending;
            let expired = res.scope.is_some()
                && (crate::api::client::is_session_expired(&res.warning)
                    || res
                        .result
                        .as_ref()
                        .err()
                        .is_some_and(|e| crate::api::client::is_session_expired(e)));
            if !res.campuses.is_empty() {
                self.osm_page.campuses = res.campuses;
                self.osm_page.selected_key = res.selected_key;
                if let Some(session) = &self.session {
                    let keep = self
                        .config
                        .school_campus
                        .as_ref()
                        .map(|s| s.school == session.unid && s.key == self.osm_page.selected_key)
                        .unwrap_or(false);
                    if !keep {
                        self.config.school_campus = Some(crate::api::campus::Selection {
                            school: session.unid.clone(),
                            key: self.osm_page.selected_key.clone(),
                            confirmed: false,
                        });
                    }
                }
            }
            self.osm_page.offline = res.offline;
            if let Some(inputs) = res.inputs {
                self.run_page.preview_inputs = Some(std::sync::Arc::new(inputs));
            }
            match res.result {
                Ok(net) => {
                    let nodes = net.graph.node_count();
                    let bld = net.buildings.len();
                    self.osm_page.fitted = false;
                    self.osm_page.status = format!(
                        "√ {}路网加载成功：{nodes} 节点 / {bld} 建筑",
                        if self.osm_page.offline { "离线" } else { "" }
                    );
                    if !res.warning.is_empty() {
                        self.osm_page.status.push_str(&format!("\n{}", res.warning));
                    }
                    self.log
                        .push(&format!("[osm] 路网加载成功：{nodes} 节点 / {bld} 建筑"));
                    self.network = Some(net);
                    self.run_page.preview_stale = true;
                    // 中间结果（pending）只用于先显示地图；缓存列表、文件大小与 GPX 分析在最终结果到达后再做
                    if !res.pending {
                        self.osm_page.remember_file_size();
                        self.refresh_cache_entries();
                        self.analyze_gpx();
                    }
                }
                Err(e) => {
                    self.network = None;
                    self.run_page.preview = None;
                    self.run_page.preview_stale = true;
                    self.osm_page.status = crate::road_status::explain(&e);
                    self.log.push(&format!("[osm] 路网加载失败：{e}"));
                }
            }
            if expired {
                self.expire_road_session();
            } else if refresh_manual_metadata {
                self.refresh_points();
            }
        }
    }
}

fn fmt_hms(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

fn fmt_dur(sec: i64) -> String {
    format!("{}:{:02}:{:02}", sec / 3600, sec % 3600 / 60, sec % 60)
}

fn run_popup(v: &serde_json::Value) -> PopupInfo {
    let dist = v["dist"].as_f64().unwrap_or(0.0);
    let dur = v["dur"].as_i64().unwrap_or(0);
    let pace = if dist > 0.0 {
        dur as f64 / (dist / 1000.0)
    } else {
        0.0
    };
    let mut lines = vec![
        format!("记录号：{}", v["rrid"].as_i64().unwrap_or(0)),
        format!("UUID：{}", v["uuid"].as_str().unwrap_or("")),
        format!(
            "距离：{:.2} km（达标线 {} m）",
            dist / 1000.0,
            v["sel_distance"].as_i64().unwrap_or(0)
        ),
        format!(
            "时长：{} · 配速 {}:{:02}/km",
            fmt_dur(dur),
            (pace / 60.0) as i64,
            (pace as i64) % 60
        ),
        format!(
            "步数：{}（步频 {} spm）",
            v["steps"].as_i64().unwrap_or(0),
            v["avg_step_freq"].as_i64().unwrap_or(0)
        ),
        format!(
            "卡路里：{} kcal · 功率 {} W",
            v["calorie"].as_i64().unwrap_or(0),
            v["avg_power"].as_i64().unwrap_or(0)
        ),
        format!("开始时间：{}", fmt_hms(v["start"].as_i64().unwrap_or(0))),
        "记录提交：成功".into(),
        format!("OBS 上传：{}/2", v["obs_ok"].as_i64().unwrap_or(0)),
    ];
    if let Ok(verification) =
        serde_json::from_value::<crate::api::flow::Verification>(v["verification"].clone())
    {
        lines.push(format!(
            "详情读取：{}",
            if verification.detail.is_some() {
                "成功"
            } else {
                "失败"
            }
        ));
        lines.push(format!("服务端达标判定：{}", verification.label()));
        if let Some(error) = verification.error {
            lines.push(format!("详情错误：{error}"));
        }
        if let Some(list) = verification
            .detail
            .as_ref()
            .and_then(|d| d.get("reasonList"))
            .and_then(|v| v.as_array())
        {
            for r in list {
                let label = match r.get("complete").and_then(|v| v.as_bool()) {
                    Some(true) => "达标",
                    Some(false) => "未达标",
                    None => "未知",
                };
                lines.push(format!(
                    "判定：{} {}",
                    label,
                    r["reason"].as_str().unwrap_or("")
                ));
            }
        }
    } else {
        lines.push("详情读取：未知；服务端达标判定：未知".into());
    }
    PopupInfo {
        title: "跑步结果".into(),
        lines,
    }
}

fn ai_popup(v: &serde_json::Value) -> PopupInfo {
    if v["batch"].as_bool().unwrap_or(false) {
        let mut lines = vec![
            format!(
                "成功：{}/{}",
                v["success"].as_i64().unwrap_or(0),
                v["total"].as_i64().unwrap_or(0)
            ),
            format!(
                "覆盖：{} 天 × 每天 {} 次 × {} 个项目",
                v["days"].as_i64().unwrap_or(0),
                v["per_day"].as_i64().unwrap_or(0),
                v["sports"].as_i64().unwrap_or(0)
            ),
        ];
        if let Some(reason) = v["stopped"].as_str() {
            lines.push(format!(
                "已中止：{reason}（已处理 {}/{} 条，其余未提交）",
                v["done"].as_i64().unwrap_or(0),
                v["total"].as_i64().unwrap_or(0)
            ));
        }
        return PopupInfo {
            title: "AI 批量补签结果".into(),
            lines,
        };
    }
    PopupInfo {
        title: "AI 运动提交结果".into(),
        lines: vec![
            format!("项目 sportId：{}", v["sport_id"].as_i64().unwrap_or(0)),
            format!("模式：{}", v["mode"].as_str().unwrap_or("")),
            format!("成绩：{}", v["score"].as_str().unwrap_or("")),
            format!(
                "用时：{:.1} 秒 · 速度 {}/分 · 消耗 {:.1}",
                v["secs"].as_f64().unwrap_or(0.0),
                v["per_min"].as_i64().unwrap_or(0),
                v["consume"].as_f64().unwrap_or(0.0),
            ),
            format!(
                "服务器：{} {}",
                v["resp_error"].as_i64().unwrap_or(0),
                v["resp_msg"].as_str().unwrap_or("")
            ),
        ],
    }
}

#[cfg(test)]
mod result_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn popup_separates_partial_upload_reading_and_grade() {
        let failed = crate::api::flow::Verification::from_result(Ok(
            json!({"complete":false,"reasonList":[{"complete":false,"reason":"点位未经过"}]}),
        ));
        let first = run_popup(&json!({"rrid":1,"obs_ok":1,"verification":failed}))
            .lines
            .join("\n");
        assert!(first.contains("OBS 上传：1/2"));
        assert!(first.contains("详情读取：成功"));
        assert!(first.contains("服务端达标判定：未达标"));
        let next = crate::api::flow::Verification::from_result(Err("offline".into()));
        let second = run_popup(&json!({"rrid":2,"obs_ok":2,"verification":next}))
            .lines
            .join("\n");
        assert!(second.contains("详情读取：失败"));
        assert!(second.contains("服务端达标判定：未知"));
        assert!(!second.contains("点位未经过"));
    }
}
