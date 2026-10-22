//! 后台任务与消息处理：IP 获取 / 登录 / 刷新 / 消息协议分发。

use super::{App, UiEvent};
use crate::api::model;
use crate::track::generate_road::RouteMode;
use crate::update::ReleaseInfo;

impl App {
    /// 任一后台任务进行中（更新前的打断保护）。
    pub fn any_busy(&self) -> bool {
        self.login_busy
            || self.run_busy
            || self.ai_busy
            || self.ai_list_busy
            || self.records_busy
            || self.ai_records_busy
            || self.semester_busy
            || self.data_page.cheat_checking
            || self.rank_busy
            || self.user_busy
            || self.osm_page.busy
            || self.run_page.preview_busy
    }

    /// 检查更新；manual=true 时切到关于页并显示状态。
    pub fn check_update(&mut self, manual: bool) {
        if self.update.checking || self.update.downloading {
            return;
        }
        self.update.checking = true;
        self.update.check_error = None;
        if manual {
            self.tab = 8;
            self.status = "正在检查更新…".into();
        }
        self.spawn_job(move |tx| {
            let mut log = |s: &str| tx.log(s);
            let result = crate::update::check_latest(&mut log);
            tx.send(UiEvent::UpdateCheck(result)).ok();
        });
    }

    /// 下载并应用更新（桌面：解包替换 exe；Android：APK 落私有目录）。
    pub fn start_update_download(&mut self, rel: ReleaseInfo) {
        if self.update.downloading {
            return;
        }
        self.update.downloading = true;
        self.update.done = 0;
        self.update.total = None;
        self.status = format!("正在下载 {}…", rel.tag);
        self.spawn_job(move |tx| {
            let outcome: Result<String, String> = (|| {
                let mut report = |done: u64, total: Option<u64>| {
                    tx.send(UiEvent::UpdateProgress {
                        done,
                        total: total.filter(|t| *t > 0),
                    })
                    .ok();
                };
                let bytes = crate::update::download_verified(&rel, &mut report)?;
                #[cfg(target_os = "android")]
                {
                    let path = crate::platform::data_dir().join(crate::update::APK_NAME);
                    std::fs::write(&path, &bytes).map_err(|e| format!("保存安装包失败: {e}"))?;
                    Ok(rel.tag.clone())
                }
                #[cfg(not(target_os = "android"))]
                {
                    let bin = crate::update::extract(&rel.asset_name, &bytes)?;
                    crate::update::apply(&bin)?;
                    Ok(rel.tag.clone())
                }
            })();
            tx.send(UiEvent::UpdateDone(outcome)).ok();
        });
    }

    /// 后台拉取实时点位并落盘（供路线预览使用），完成后触发预览刷新。
    /// 复用 fetch_points 的 TTL 缓存，仅在 Road 模式且锚点已配置时拉取。
    pub(crate) fn refresh_points(&mut self) {
        self.refresh_school_route(false);
    }

    pub(crate) fn refresh_school_route(&mut self, force: bool) {
        if self.run_page.route_mode != RouteMode::Road {
            return;
        }
        let Some(session) = self.session.clone() else {
            if !self.config.osm_path.is_empty() {
                self.load_osm_async(self.config.osm_path.clone());
            } else {
                self.osm_page.status = "请先登录，才能获取学校路网".into();
            }
            return;
        };
        // Metadata refresh must not discard an already parsed manual map.
        let manual_network = if !self.config.osm_path.is_empty() {
            self.network.clone()
        } else {
            None
        };
        let request_id = self.begin_network_load();
        self.network = manual_network.clone();
        let identity = self.identity.clone();
        let anchor = identity.anchor_coordinate().ok();
        let preview_anchor = anchor.filter(|_| !identity.has_unconfigured_default_location());
        let scope = Some((
            session.uid,
            session.unid.clone(),
            anchor,
            session.token.clone(),
        ));
        let path = self.config.osm_path.clone();
        let selection = self.config.school_campus.clone();
        let control = self.osm_page.cancel.clone();
        let net_tx = self.net_tx.clone();
        #[cfg(test)]
        let route_fixture = self.route_fixture.clone();
        self.osm_page.status = "正在获取学校点位、围栏及路网…".into();
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            #[cfg(test)]
            if let Some((host, data_dir)) = route_fixture {
                client.set_test_host(host);
                crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(data_dir));
            }
            let mut loaded = manual_network;
            let mut inputs = None;
            let mut campuses = Vec::new();
            let mut selected_key = String::new();
            let stage = |message: &str| {
                let _ = tx.send(UiEvent::RoadStage {
                    req: request_id,
                    stage: message.to_string(),
                });
            };
            let result = (|| {
                control.check()?;
                if !path.trim().is_empty() && loaded.is_none() {
                    let net =
                        std::sync::Arc::new(crate::track::generate_road::load_network_path(&path)?);
                    loaded = Some(net.clone());
                    let _ = net_tx.send(super::osm::NetworkLoad {
                        pending: true,
                        warning: "本地路网已读取；正在获取学校围栏与点位…".into(),
                        offline: false,
                        request_id,
                        campuses: vec![],
                        selected_key: String::new(),
                        scope: scope.clone(),
                        inputs: None,
                        result: Ok(net),
                    });
                }
                stage("获取当前学校围栏…");
                let response = crate::api::fence::fetch_geo_fence_response(&mut client)?;
                campuses = crate::api::campus::catalog(&response);
                let school = client.login.as_ref().ok_or("未登录")?.unid.clone();
                let campus = crate::api::campus::chosen(&campuses, selection.as_ref(), &school)
                    .or_else(|| {
                        campuses.iter().find(|c| {
                            preview_anchor.is_some_and(|anchor| {
                                c.polygons.iter().any(|p| {
                                    route_planner::point_in_polygon(
                                        &p.iter()
                                            .map(|&(a, b)| route_planner::Coord::new(b, a))
                                            .collect::<Vec<_>>(),
                                        route_planner::Coord::new(
                                            anchor.longitude,
                                            anchor.latitude,
                                        ),
                                    )
                                })
                            })
                        })
                    })
                    .or_else(|| campuses.first())
                    .cloned();
                let fences = campus
                    .as_ref()
                    .map(|c| c.polygons.clone())
                    .unwrap_or_default();
                let run_area_id = if let Some(campus) = &campus {
                    selected_key = campus.key.clone();
                    campus.run_area_id(response.area.run_area_id)?
                } else {
                    (response.area.run_area_id >= 0).then(|| response.area.run_area_id.to_string())
                };
                control.check()?;
                stage("下载或读取学校路网…");
                if loaded.is_none() && campus.is_some() {
                    loaded = Some(std::sync::Arc::new(
                        crate::api::school_roads::load_for_school_cancellable(
                            &path, &school, &fences, force, &control, &mut log,
                        )?,
                    ));
                }
                control.check()?;
                if let Some(network) = &loaded {
                    let _ = net_tx.send(super::osm::NetworkLoad {
                        pending: true,
                        warning: "路网已读取；正在刷新路线点位…".into(),
                        offline: false,
                        request_id,
                        campuses: campuses.clone(),
                        selected_key: selected_key.clone(),
                        scope: scope.clone(),
                        inputs: None,
                        result: Ok(network.clone()),
                    });
                }
                // Preview metadata is optional for displaying the road graph.
                let anchor =
                    preview_anchor.ok_or("请在设备信息页配置当前城市和定位锚点，再刷新路线点位")?;
                stage("获取当前学校策略与路线点位…");
                let policy = crate::api::policy::fetch_policy(&mut client)?;
                control.check()?;
                let pts = crate::api::points::fetch_points_ext(
                    &mut client,
                    anchor,
                    run_area_id,
                    &mut log,
                )?;
                if pts.is_empty() || crate::api::points::points_bd(&pts).len() != pts.len() {
                    return Err("点位为空或坐标无效".into());
                }
                let route_points = crate::api::points::route_points(&policy.must_points, &pts);
                if loaded.is_none() {
                    stage("按学校真实点位获取附近路网…");
                    loaded = Some(std::sync::Arc::new(
                        crate::api::school_roads::load_for_route_cancellable(
                            &path,
                            &school,
                            &fences,
                            &route_points,
                            force,
                            &control,
                            &mut log,
                        )?,
                    ));
                }
                let session = client.login.as_ref().ok_or("未登录")?;
                inputs = Some(super::run::PreviewInputs {
                    uid: session.uid,
                    unid: session.unid.clone(),
                    anchor,
                    points: route_points,
                    fences: fences.clone(),
                    loaded_ms: crate::crypto::envelope::now_ms(),
                });
                control.check()?;
                if path.is_empty() {
                    let area_name = campus
                        .as_ref()
                        .map(|c| c.name.as_str())
                        .unwrap_or("点位附近道路（无学校边界）");
                    let label = session
                        .profile
                        .get("campusName")
                        .and_then(|v| v.as_str())
                        .map(|school| format!("{school} / {area_name}"))
                        .unwrap_or_else(|| area_name.to_owned());
                    crate::api::school_roads::save_context(
                        &session.unid,
                        &label,
                        &fences,
                        &inputs.as_ref().unwrap().points,
                    );
                }
                Ok(())
            })();
            let (result, warning) = finish_road_load(loaded, result);
            let _ = net_tx.send(super::osm::NetworkLoad {
                pending: false,
                warning,
                offline: false,
                request_id,
                campuses,
                selected_key,
                scope,
                inputs,
                result,
            });
        });
    }

    pub(crate) fn fetch_ip(&mut self) {
        self.spawn_job(|tx| {
            let agent = crate::api::client::make_agent();
            let mut log = App::logger(tx.clone());
            let mut ip = String::new();
            for url in [
                "https://api.ipify.org?format=json",
                "https://httpbin.org/ip",
            ] {
                if let Ok(resp) = agent.get(url).call() {
                    if let Ok(text) = resp.into_string() {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            let got = v
                                .get("ip")
                                .or_else(|| v.get("origin"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !got.is_empty() {
                                ip = got;
                                break;
                            }
                        }
                    }
                }
            }
            if ip.is_empty() {
                log("公网 IP 获取失败");
                tx.send(UiEvent::Ip("失败".into())).ok();
            } else {
                log(&format!("公网 IP：{ip}"));
                tx.send(UiEvent::Ip(ip)).ok();
            }
        });
    }

    pub(crate) fn do_login(&mut self) {
        if self.username.is_empty() || self.password.is_empty() {
            self.status = "请填写账号和密码".into();
            return;
        }
        let username = self.username.trim().to_string();
        let password = self.password.clone();
        let identity = self.identity.clone();
        let remember = self.remember;
        self.config.username = username.clone();
        self.config.password = if remember {
            password.clone()
        } else {
            String::new()
        };
        self.config.remember = remember;
        let _ = model::save_config(&self.config);
        self.renew_session_jobs();
        // Retire requests using the old token before a login worker can save a new session.
        self.osm_page.cancel.cancel();
        self.osm_page.request_id = self.osm_page.request_id.wrapping_add(1);
        self.osm_page.busy = false;
        self.run_page.preview_cancel.cancel();
        self.login_busy = true;
        self.status = format!("登录中（{username}）…");
        self.spawn_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, None);
            let result = match crate::api::login::login(&mut client, &username, &password, &mut log)
            {
                Ok(mut sess) => {
                    log(&format!(
                        "√ 登录成功 uid={} unid={} name={}",
                        sess.uid, sess.unid, sess.name
                    ));
                    sess.username = username;
                    Ok(sess)
                }
                Err(e) => {
                    log(&format!("× 登录失败: {e}"));
                    Err(e)
                }
            };
            tx.send(UiEvent::Login(result)).ok();
        });
    }

    pub(crate) fn do_logout(&mut self) {
        self.renew_session_jobs();
        let identity = self.identity.clone();
        let session = self.session.clone();
        self.session = None;
        self.auth_expired = false;
        self.begin_network_load();
        self.osm_page.busy = false;
        self.spawn_job(move |tx| {
            let mut log = App::logger(tx.clone());
            if let Some(sess) = session {
                let mut client = crate::api::client::ApiClient::new(identity, Some(sess));
                crate::api::login::logout(&mut client, &mut log);
            }
        });
        self.status = "已登出".into();
    }

    pub fn refresh_ai_list(&mut self) {
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        let identity = self.identity.clone();
        let req = self.reqs.issue();
        self.reqs.ai_list = req;
        self.ai_list_busy = true;
        self.status = "AI 列表拉取中…".into();
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let result = crate::api::flow::run_ai_list(&mut client, &mut log);
            if let Err(e) = &result {
                log(&format!("× AI 列表拉取失败: {e}"));
            }
            tx.send(UiEvent::AiList { req, result }).ok();
        });
    }

    pub fn refresh_records(&mut self) {
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        let identity = self.identity.clone();
        let req = self.reqs.issue();
        self.reqs.records = req;
        self.records_busy = true;
        self.status = "记录拉取中…".into();
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let result = crate::api::flow::run_records(&mut client, &mut log);
            if let Err(e) = &result {
                log(&format!("× 记录拉取失败: {e}"));
            }
            tx.send(UiEvent::Records { req, result }).ok();
        });
    }

    /// 我的页数据聚合。
    pub fn refresh_user_page(&mut self) {
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        let identity = self.identity.clone();
        let req = self.reqs.issue();
        self.reqs.user = req;
        self.user_busy = true;
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let info = crate::api::user::fetch_my_info(&mut client, &mut log);
            log(&info.refresh_message());
            tx.send(UiEvent::User { req, info }).ok();
        });
    }

    /// 拉取单条跑步详情。
    pub fn fetch_run_detail(&mut self, rrid: i64) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let identity = self.identity.clone();
        self.records_page.detail_loading = true;
        #[cfg(test)]
        let detail_test_host = self.route_fixture.as_ref().map(|(host, _)| host.clone());
        self.spawn_session_job(move |tx| {
            let mut client = tx.client(identity, Some(session));
            #[cfg(test)]
            if let Some(host) = detail_test_host {
                client.set_test_host(host);
            }
            let raw = match crate::api::records::fetch_detail(&mut client, rrid) {
                Ok(d) => serde_json::to_value(&d).unwrap_or_default(),
                Err(e) => {
                    serde_json::json!({ "rrid": rrid, "statusInfo": format!("详情拉取失败：{e}") })
                }
            };
            tx.send(UiEvent::RunDetail(raw)).ok();
        });
    }

    /// 拉取单条 AI 记录全量详情。
    pub fn fetch_ai_detail(&mut self, id: i64) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let identity = self.identity.clone();
        self.records_page.ai_detail_loading = true;
        self.spawn_session_job(move |tx| {
            let mut client = tx.client(identity, Some(session));
            let raw = match crate::api::ai::fetch_record_detail(&mut client, id) {
                Ok(d) => d,
                Err(e) => serde_json::json!({ "fetchError": e }),
            };
            tx.send(UiEvent::AiDetail { id, raw }).ok();
        });
    }

    /// AI 记录：遍历全部项目合并（按天排序）。
    pub fn refresh_ai_records(&mut self) {
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        let identity = self.identity.clone();
        let req = self.reqs.issue();
        self.reqs.ai_records = req;
        self.ai_records_busy = true;
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            // 先拉项目列表
            let list = match crate::api::ai::fetch_list(&mut client) {
                Ok(l) => l,
                Err(e) => {
                    log(&format!("× [ai] 项目列表拉取失败: {e}"));
                    tx.send(UiEvent::AiRecords {
                        req,
                        result: Err(e),
                    })
                    .ok();
                    return;
                }
            };
            let mut all_groups: Vec<crate::api::ai::AiRecordGroup> = Vec::new();
            let mut total = 0i64;
            for sport in &list {
                match crate::api::ai::fetch_records(&mut client, sport.id, 50) {
                    Ok(page) => {
                        for mut g in page.groups {
                            for r in &mut g.records {
                                r.name = format!("{} {}", sport.name, r.name);
                            }
                            all_groups.push(g);
                        }
                        total += page.total_count;
                    }
                    Err(e) => {
                        log(&format!("× [ai] {} 拉取失败: {e}", sport.name));
                        tx.send(UiEvent::AiRecords {
                            req,
                            result: Err(e),
                        })
                        .ok();
                        return;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            all_groups.sort_by_key(|g| std::cmp::Reverse(g.score_date));
            let count: usize = all_groups.iter().map(|g| g.records.len()).sum();
            log(&format!("√ [ai] {} 个项目共 {} 条记录", list.len(), count));
            tx.send(UiEvent::AiRecords {
                req,
                result: Ok((all_groups, total)),
            })
            .ok();
        });
    }

    /// 学期 + 违规自查（登录 / 跑步提交成功后自动调用）。
    ///
    /// 两项各自独立的任务与忙碌标记；已在进行的一项不会重复发起。
    pub fn refresh_data_page(&mut self) {
        if self.session.is_none() {
            self.status = "请先登录".into();
            return;
        }
        self.refresh_semester();
        self.refresh_cheat_only();
    }

    pub fn refresh_semester(&mut self) {
        if self.semester_busy {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        let identity = self.identity.clone();
        let req = self.reqs.issue();
        self.reqs.semester = req;
        self.semester_busy = true;
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let result = match crate::api::semester::query(&mut client, &mut log) {
                Ok(r) => {
                    if let Some(s) = &r.summary {
                        log(&format!(
                            "√ [semester] {} 有效 {}/{} 次，有效里程 {:.2} km",
                            s.sname,
                            s.semester_valid_count,
                            s.semester_count,
                            s.semester_valid_dis / 1000.0
                        ));
                    }
                    Ok(r.summary)
                }
                Err(e) => {
                    log(&format!("× [semester] 拉取失败: {e}"));
                    Err(e)
                }
            };
            tx.send(UiEvent::Semester { req, result }).ok();
        });
    }

    pub fn refresh_cheat_only(&mut self) {
        if self.data_page.cheat_checking {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        self.data_page.cheat_checking = true;
        self.data_page.cheat_error = None;
        self.data_page.cheat = None;
        let identity = self.identity.clone();
        let req = self.reqs.issue();
        self.reqs.cheat = req;
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let result = crate::api::cheat::query(&mut client, 1, &mut log);
            if let Err(e) = &result {
                log(&format!("× [cheat] 检查失败: {e}"));
            }
            tx.send(UiEvent::Cheat { req, result }).ok();
        });
    }

    /// 榜单查询：kind=main|indoor|history。
    pub fn refresh_rank(&mut self, kind: &str, subtype: i64) {
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        let identity = self.identity.clone();
        let kind = kind.to_string();
        let req = self.reqs.issue();
        self.reqs.rank = req;
        self.rank_busy = true;
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let res: Result<Vec<crate::api::rank::RankRow>, String> = (|| {
                match kind.as_str() {
                    "main" => {
                        // 当天无数据时自动回退最近 3 天（当日累计榜凌晨常为空）
                        let (rtype, sort_type) = (subtype / 10, subtype % 10);
                        let mut rows =
                            crate::api::rank::main_rank(&mut client, rtype, sort_type, None, None)?;
                        let mut back = 0;
                        while rows.is_empty() && back < 3 {
                            back += 1;
                            let d = (chrono::Local::now() - chrono::Duration::days(back))
                                .format("%Y-%m-%d")
                                .to_string();
                            log(&format!("[rank] 当天暂无数据，回退查询 {d}"));
                            rows = crate::api::rank::main_rank(
                                &mut client,
                                rtype,
                                sort_type,
                                None,
                                Some(d),
                            )?;
                        }
                        Ok(rows)
                    }
                    "indoor" => crate::api::rank::indoor_rank(&mut client, subtype, -1),
                    _ => crate::api::rank::history_rank(&mut client, subtype, -1),
                }
            })();
            match &res {
                Ok(rows) => log(&format!("[rank] {} 行", rows.len())),
                Err(e) => log(&format!("× [rank] 查询失败: {e}")),
            }
            tx.send(UiEvent::Rank { req, result: res }).ok();
        });
    }
}

// Once map data is available, a metadata failure must not turn it into a map-load failure.
fn finish_road_load(
    loaded: Option<std::sync::Arc<route_planner::RoadGraph>>,
    metadata: Result<(), String>,
) -> (
    Result<std::sync::Arc<route_planner::RoadGraph>, String>,
    String,
) {
    match (loaded, metadata) {
        (Some(net), Ok(())) => (Ok(net), String::new()),
        (Some(net), Err(error)) => (
            Ok(net),
            format!("路线预览暂不可用：{error}；请在跑步页刷新围栏与点位"),
        ),
        (None, Err(error)) => (Err(error), String::new()),
        (None, Ok(())) => (Err("路网数据为空".into()), String::new()),
    }
}

#[cfg(test)]
mod road_load_tests {
    use super::*;
    #[test]
    fn metadata_failure_keeps_loaded_roads_but_download_failure_stays_an_error() {
        let net = std::sync::Arc::new(route_planner::RoadGraph::new());
        for error in ["定位未配置", "策略请求失败", "点位请求失败", "围栏请求失败"]
        {
            let (result, warning) = finish_road_load(Some(net.clone()), Err(error.into()));
            assert!(std::sync::Arc::ptr_eq(&result.unwrap(), &net));
            assert!(warning.contains(error));
        }
        assert!(finish_road_load(None, Err("下载失败".into())).0.is_err());
    }
}
