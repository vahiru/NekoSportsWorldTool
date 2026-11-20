//! Stable C ABI. The host sends JSON; only allowlisted commands can run.
//! Each worker has an account-scoped data directory and explicit session.
use crate::{api, api::client::ApiClient, api::model, task_control::Control};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::{c_char, CStr, CString},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Default)]
struct Job {
    logs: Vec<String>,
    result: Option<Result<Value, String>>,
    control: Arc<Control>,
}
static JOBS: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<Job>>>>> = OnceLock::new();
static SCOPES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
fn jobs() -> &'static Mutex<BTreeMap<String, Arc<Mutex<Job>>>> {
    JOBS.get_or_init(Default::default)
}
fn response(result: Result<Value, String>) -> Value {
    match result {
        Ok(data) => json!({"ok": true, "data": data}),
        Err(error) => {
            json!({"ok": false, "auth_expired": api::client::is_session_expired(&error), "error": error})
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn neko_call(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        if input.is_null() {
            return Err("请求为空".into());
        }
        let raw = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|_| "请求编码无效")?;
        let request: Value = serde_json::from_str(raw).map_err(|_| "请求格式无效")?;
        dispatch(request)
    });
    let out = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => response(Err(error)),
        Err(_) => response(Err("运动核心发生错误，请重新打开页面".into())),
    };
    CString::new(out.to_string())
        .expect("JSON escapes null bytes")
        .into_raw()
}
#[no_mangle]
pub unsafe extern "C" fn neko_free(pointer: *mut c_char) {
    if !pointer.is_null() {
        drop(unsafe { CString::from_raw(pointer) });
    }
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("缺少 {key}"))
}
fn integer(v: &Value, key: &str, default: i64, min: i64, max: i64) -> Result<i64, String> {
    let n = match v.get(key) {
        None | Some(Value::Null) => default,
        Some(value) => value.as_i64().ok_or_else(|| format!("{key} 必须为整数"))?,
    };
    if !(min..=max).contains(&n) {
        return Err(format!("{key} 超出范围 {min}–{max}"));
    }
    Ok(n)
}
fn number(v: &Value, key: &str, default: f64, min: f64, max: f64) -> Result<f64, String> {
    let n = match v.get(key) {
        None | Some(Value::Null) => default,
        Some(value) => value.as_f64().ok_or_else(|| format!("{key} 必须为数字"))?,
    };
    if !n.is_finite() || !(min..=max).contains(&n) {
        return Err(format!("{key} 超出范围 {min}–{max}"));
    }
    Ok(n)
}
fn dispatch(v: Value) -> Result<Value, String> {
    match text(&v, "op")? {
        "start" => {
            let directory = PathBuf::from(text(&v, "directory")?);
            if !directory.is_absolute() {
                return Err("必须使用应用私有目录".into());
            }
            std::fs::create_dir_all(&directory)
                .map_err(|e| format!("无法建立运动数据目录：{e}"))?;
            let command = text(&v, "command")?.to_owned();
            if !COMMANDS.contains(&command.as_str()) {
                return Err("未知运动命令".into());
            }
            let session = v
                .get("session")
                .filter(|s| !s.is_null())
                .map(|s| serde_json::from_value::<model::Session>(s.clone()))
                .transpose()
                .map_err(|_| "会话格式无效")?;
            let args = v.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let id = uuid::Uuid::new_v4().to_string();
            let job = Arc::new(Mutex::new(Job::default()));
            let scope = SCOPES
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .entry(directory.clone())
                .or_default()
                .clone();
            {
                let mut all = jobs().lock().unwrap();
                if all.len() >= 128 {
                    let completed: Vec<_> = all
                        .iter()
                        .filter(|(_, j)| j.lock().unwrap().result.is_some())
                        .map(|(k, _)| k.clone())
                        .collect();
                    for key in completed {
                        all.remove(&key);
                    }
                    if all.len() >= 128 {
                        return Err("运动任务过多，请等待当前任务完成".into());
                    }
                }
                all.insert(id.clone(), job.clone());
            }
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _guard = scope.lock().map_err(|_| "运动数据目录被占用，请重启应用")?;
                    crate::platform::set_data_dir(directory);
                    let control = job.lock().unwrap().control.clone();
                    control.check()?;
                    let mut log = |line: &str| {
                        let mut j = job.lock().unwrap();
                        if j.logs.len() < 2000 {
                            j.logs.push(line.to_string());
                        }
                    };
                    execute(&command, &args, session, &control, &mut log).map(|value| {
                        if command == "login" {
                            value
                        } else {
                            strip_credentials(value)
                        }
                    })
                }))
                .unwrap_or_else(|_| Err("运动核心发生错误，本次任务已停止".into()));
                job.lock().unwrap().result = Some(result);
            });
            Ok(json!({"ok": true, "id": id}))
        }
        "poll" | "cancel" | "forget" => {
            let id = text(&v, "id")?;
            let mut all = jobs().lock().unwrap();
            let job = all.get(id).ok_or("任务已结束或不存在")?.clone();
            let j = job.lock().unwrap();
            match v["op"].as_str().unwrap() {
                "poll" => {
                    let cursor = v["cursor"].as_u64().unwrap_or(0) as usize;
                    Ok(
                        json!({"ok": true, "done": j.result.is_some(), "cancellable": j.result.is_none() && j.control.cancellable(),
                        "logs": j.logs.get(cursor..).unwrap_or(&[]), "cursor": j.logs.len(),
                        "result": j.result.clone().map(response)}),
                    )
                }
                "cancel" => {
                    Ok(json!({"ok": true, "cancelled": j.result.is_none() && j.control.cancel()}))
                }
                _ => {
                    if j.result.is_none() {
                        return Err("任务仍在运行".into());
                    }
                    all.remove(id);
                    Ok(json!({"ok": true}))
                }
            }
        }
        _ => Err("未知桥接操作".into()),
    }
}
const COMMANDS: &[&str] = &[
    "bootstrap",
    "settings",
    "login",
    "logout",
    "semester",
    "profile",
    "records",
    "record",
    "ai_list",
    "ai_info",
    "ai_records",
    "ai_record",
    "ai_submit",
    "ai_batch",
    "rank",
    "cheat",
    "campuses",
    "select_campus",
    "preview",
    "run",
];

fn execute(
    command: &str,
    args: &Value,
    session: Option<model::Session>,
    control: &Control,
    log: &mut dyn FnMut(&str),
) -> Result<Value, String> {
    let mut identity = model::try_load_identity()?;
    let persisted = crate::state::read::<model::HeaderIdentity>()?
        .ok_or("无法持久化设备身份，请检查应用存储空间")?;
    if persisted.device_id != identity.device_id
        || persisted.app_install_time != identity.app_install_time
    {
        return Err("设备身份未能保存，已停止请求，请检查应用存储空间".into());
    }
    if command == "bootstrap" {
        let mut config = model::load_config();
        config.password.clear();
        config.remember = false;
        return Ok(
            json!({"identity": identity, "config": config, "version": env!("CARGO_PKG_VERSION")}),
        );
    }
    if command == "settings" {
        if let Some(changes) = args.get("identity").and_then(Value::as_object) {
            let mut value = serde_json::to_value(&identity).map_err(|e| e.to_string())?;
            for key in [
                "platform",
                "device_name",
                "manufacturer",
                "os_version",
                "city",
                "anchor_lat",
                "anchor_lon",
            ] {
                if let Some(v) = changes.get(key) {
                    value[key] = v.clone();
                }
            }
            identity = serde_json::from_value(value).map_err(|_| "设备信息格式无效")?;
            if !["android", "ios"].contains(&identity.platform.as_str()) {
                return Err("设备平台无效".into());
            }
            identity.anchor_coordinate()?;
            if identity.city.trim().is_empty() {
                return Err("请填写所在城市".into());
            }
            model::save_identity(&identity)?;
        }
        if let Some(changes) = args.get("config").and_then(Value::as_object) {
            let mut value =
                serde_json::to_value(model::load_config()).map_err(|e| e.to_string())?;
            for key in [
                "dist_min",
                "dist_max",
                "pace_min",
                "pace_max",
                "face_check",
                "manual_altitude",
                "manual_altitude_range",
                "route_mode",
                "terrain_simulation",
            ] {
                if let Some(v) = changes.get(key) {
                    value[key] = v.clone();
                }
            }
            let mut config: model::Config =
                serde_json::from_value(value).map_err(|_| "跑步设置格式无效")?;
            if let Some(spec) = args["altitude_spec"].as_str() {
                (config.manual_altitude, config.manual_altitude_range) =
                    match crate::track::altitude::parse_spec(spec)? {
                        None => (None, None),
                        Some(crate::track::altitude::AltitudeSpec::Single(value)) => {
                            (Some(value), None)
                        }
                        Some(crate::track::altitude::AltitudeSpec::Range(value)) => {
                            (None, Some(value))
                        }
                    };
            }
            config.password.clear();
            config.remember = false;
            model::save_config(&config)?;
        }
        return Ok(json!({"identity": identity, "config": model::load_config()}));
    }
    let mut client = ApiClient::new(identity, session).with_api_mode(model::load_config().api_mode);
    if command == "login" {
        let username = text(args, "username")?.trim();
        let password = text(args, "password")?;
        let session = api::login::login(&mut client, username, password, log)?;
        return serde_json::to_value(session).map_err(|e| e.to_string());
    }
    if !client
        .login
        .as_ref()
        .is_some_and(model::Session::is_logged_in)
    {
        return Err(api::client::SESSION_EXPIRED.into());
    }
    match command {
        "logout" => {
            api::login::request_logout(&mut client, log);
            Ok(Value::Null)
        }
        "semester" => {
            let result = api::semester::query(&mut client, log)?;
            Ok(json!({"summary": result.summary, "personal": result.personal_raw}))
        }
        "profile" => {
            let result = api::user::fetch_my_info(&mut client, log);
            Ok(
                json!({"profile": result.profile, "home_page": result.home_page, "personal_semester": result.personal_semester,
                "summary": result.summary, "completed": result.completed, "errors": result.errors}),
            )
        }
        "records" => serde_json::to_value(api::records::fetch_records(&mut client)?)
            .map_err(|e| e.to_string()),
        "record" => {
            api::records::fetch_one_record(&mut client, integer(args, "id", 0, 1, i64::MAX)?)
        }
        "ai_list" => {
            serde_json::to_value(api::ai::fetch_list(&mut client)?).map_err(|e| e.to_string())
        }
        "ai_info" => {
            let info = api::ai::fetch_info(&mut client, integer(args, "sport_id", 0, 1, 10000)?)?;
            Ok(json!({"type": info.sport_type, "number": info.number}))
        }
        "ai_records" => serde_json::to_value(api::ai::fetch_records(
            &mut client,
            integer(args, "sport_id", 0, 1, 10000)?,
            integer(args, "page_size", 100, 1, 1000)?,
        )?)
        .map_err(|e| e.to_string()),
        "ai_record" => {
            api::ai::fetch_record_detail(&mut client, integer(args, "id", 0, 1, i64::MAX)?)
        }
        "ai_submit" => {
            let sport = integer(args, "sport_id", 0, 1, 10000)?;
            let mode = match args["mode"].as_str().unwrap_or("minutes") {
                "minutes" => api::ai::AiMode::Minutes {
                    minutes: integer(args, "value", 1, 1, 30)?,
                },
                "count" => {
                    let reps = integer(args, "value", 5, 5, 1000)?;
                    if reps % 5 != 0 {
                        return Err("次数应为 5 的倍数".into());
                    }
                    api::ai::AiMode::Count { reps }
                }
                _ => return Err("AI 运动模式无效".into()),
            };
            let days = integer(args, "days_ago", 0, 0, 60)?;
            let at = if days == 0 {
                None
            } else {
                Some(crate::crypto::envelope::now_ms() - days * 86_400_000)
            };
            // A cancelled queued AI job cannot reach upload. Do not retry a write.
            control.begin_submit()?;
            log("正在提交 AI 运动记录…");
            let result = api::ai::upload(&mut client, sport, mode, at)?;
            Ok(api::client::parse_data_field(&result))
        }
        "ai_batch" => {
            let ids = args["sport_ids"].as_array().ok_or("请选择运动项目")?;
            let mut sports = Vec::new();
            for id in ids {
                let id = id
                    .as_i64()
                    .filter(|id| (1..=10000).contains(id))
                    .ok_or("运动项目无效")?;
                if !sports.contains(&id) {
                    sports.push(id);
                }
            }
            let from = integer(args, "from_day", 1, 0, 60)?;
            let to = integer(args, "to_day", 7, from, 60)?;
            if sports.is_empty() || sports.len() * (to - from + 1) as usize > 1000 {
                return Err("请选择 1 至 1000 条运动记录".into());
            }
            let mode = match args["mode"].as_str().unwrap_or("minutes") {
                "minutes" => api::ai::AiMode::Minutes {
                    minutes: integer(args, "value", 1, 1, 30)?,
                },
                "count" => {
                    let reps = integer(args, "value", 5, 5, 1000)?;
                    if reps % 5 != 0 {
                        return Err("次数应为 5 的倍数".into());
                    }
                    api::ai::AiMode::Count { reps }
                }
                _ => return Err("AI 运动模式无效".into()),
            };
            let total = sports.len() * (to - from + 1) as usize;
            let now = crate::crypto::envelope::now_ms();
            let mut completed = 0;
            for day in (from..=to).rev() {
                for sport in &sports {
                    control.check()?;
                    log(&format!(
                        "[{}/{}] 提交 {} 天前的项目 {}…",
                        completed + 1,
                        total,
                        day,
                        sport
                    ));
                    // Cancellation only prevents later entries; it cannot retract an in-flight write.
                    api::ai::upload(
                        &mut client,
                        *sport,
                        mode,
                        (day != 0).then_some(now - day * 86_400_000),
                    )
                    .map_err(|e| {
                        format!("已完成 {completed}/{total}；本条结果需核对，批量任务已停止：{e}")
                    })?;
                    completed += 1;
                    log(&format!("已完成 {completed}/{total}"));
                    if completed < total {
                        for _ in 0..10 {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            control.check()?;
                        }
                    }
                }
            }
            Ok(json!({"completed":completed,"total":total}))
        }
        "rank" => {
            let gender = integer(args, "gender", 1, 0, 1)?;
            let sort = integer(args, "sort", 1, 1, 2)?;
            let rows = match args["kind"].as_str().unwrap_or("main") {
                "main" => api::rank::main_rank(
                    &mut client,
                    integer(args, "type", 1, 1, 3)?,
                    sort,
                    Some(gender),
                    args["date"].as_str().map(str::to_owned),
                )?,
                "history" => api::rank::history_rank(&mut client, sort, gender)?,
                "indoor" => {
                    api::rank::indoor_rank(&mut client, integer(args, "range", 1, 1, 3)?, gender)?
                }
                _ => return Err("榜单类型无效".into()),
            };
            serde_json::to_value(rows).map_err(|e| e.to_string())
        }
        "cheat" => {
            let report = api::cheat::query(&mut client, integer(args, "page", 1, 1, 10000)?, log)?;
            Ok(
                json!({"label": report.label(), "reason": report.reason(), "record_id": report.record_id(), "report": report}),
            )
        }
        "campuses" | "select_campus" => {
            let policy = api::policy::fetch_policy(&mut client)?;
            let fences = match policy.geo_fence {
                Some(f) => f,
                None => api::fence::fetch_geo_fence_response(&mut client)?,
            };
            let catalog = api::campus::catalog_with_areas(&fences, &policy.run_areas)?;
            let school = client.login.as_ref().unwrap().unid.clone();
            let mut config = model::load_config();
            if command == "select_campus" {
                let key = text(args, "key")?;
                let campus = catalog
                    .iter()
                    .find(|c| c.key == key)
                    .ok_or("跑区已变化，请刷新重新选择")?;
                if campus.run_area.as_ref().is_some_and(|a| !a.valid) {
                    return Err("该跑区当前无效".into());
                }
                config.school_campus = Some(api::campus::Selection {
                    school: school.clone(),
                    key: key.into(),
                    confirmed: true,
                });
                model::save_config(&config)?;
            }
            Ok(
                json!({"school": school, "selected": config.school_campus, "min_distance": policy.min_distance,
                "campuses": catalog.iter().map(|c| json!({"key": c.key, "name": c.name, "polygons": c.polygons, "valid": c.run_area.as_ref().is_none_or(|a| a.valid)})).collect::<Vec<_>>()}),
            )
        }
        "preview" | "run" => {
            let params = run_params(args)?;
            if command == "run" {
                let out = api::flow::run_full_flow_cancellable(&mut client, &params, log, control)?;
                Ok(
                    json!({"rrid": out.result.rrid, "uuid": out.result.uuid, "distance": out.result.total_dis,
                    "duration": out.result.total_time, "obs_ok": out.obs_ok, "verification": out.verification}),
                )
            } else {
                preview(&mut client, &params, control, log)
            }
        }
        _ => Err("未知运动命令".into()),
    }
}
// Query payloads may include copies of login fields. Never expose those to
// ordinary query caches; the login response alone reaches encrypted storage.
fn strip_credentials(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter_map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase().replace('_', "");
                    if [
                        "token",
                        "password",
                        "passwd",
                        "passtoken",
                        "accesstoken",
                        "refreshtoken",
                        "authorization",
                        "secret",
                        "cookie",
                        "cookies",
                        "session",
                    ]
                    .contains(&normalized.as_str())
                    {
                        None
                    } else {
                        Some((key, strip_credentials(value)))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_credentials).collect()),
        other => other,
    }
}
fn run_params(args: &Value) -> Result<api::flow::RunParams, String> {
    let distance = number(args, "distance_km", 1.0, 0.1, 42.195)? * 1000.0;
    let pace = number(args, "pace_seconds", 420.0, 180.0, 500.0)?;
    let dur = crate::track::generate_road::effective_duration(
        distance,
        (pace * distance / 1000.0).round() as i64,
    );
    let now = chrono::Local::now();
    let start_ms = match args.get("start_ms").filter(|v| !v.is_null()) {
        Some(v) => {
            let ms = v.as_i64().ok_or("开始时间格式无效")?;
            if ms < now.timestamp_millis() - 4 * 86_400_000
                || ms > crate::start_time::latest_start_ms(now.timestamp_millis(), dur)
            {
                return Err("开始时间需在近 3 天内，且开始时间加运动时长不能晚于当前时间".into());
            }
            ms
        }
        None => crate::start_time::random_ago(now.timestamp_millis(), dur, rand::random()).ms,
    };
    let config = model::load_config();
    let altitude = match args.get("altitude").filter(|v| !v.is_null()) {
        Some(v) => Some(
            v.as_f64()
                .filter(|v| v.is_finite() && (-500.0..=9000.0).contains(v))
                .ok_or("海拔应在 -500 至 9000 米之间")?,
        ),
        None => None,
    };
    let (altitude, altitude_range) = if let Some(spec) = args["altitude_spec"].as_str() {
        match crate::track::altitude::parse_spec(spec)? {
            None => (None, None),
            Some(crate::track::altitude::AltitudeSpec::Single(n)) => (Some(n), None),
            Some(crate::track::altitude::AltitudeSpec::Range(range)) => (None, Some(range)),
        }
    } else {
        (altitude, config.manual_altitude_range)
    };
    config
        .terrain_simulation
        .validate_altitude(altitude, altitude_range)?;
    let route_mode = args["route_mode"].as_str().unwrap_or(&config.route_mode);
    if !["legacy", "road"].contains(&route_mode) {
        return Err("路线算法无效".into());
    }
    let seed = args["seed"].as_u64().unwrap_or_else(rand::random);
    Ok(api::flow::RunParams {
        dist: distance,
        dur,
        start_ms,
        seed,
        face_check: i64::from(args["face_check"].as_bool().unwrap_or(config.face_check)),
        manual_altitude: altitude,
        manual_altitude_range: altitude_range,
        route_mode: crate::track::generate_road::RouteMode::from_str(route_mode),
        terrain: config.terrain_simulation.load()?,
    })
}
fn preview(
    client: &mut ApiClient,
    params: &api::flow::RunParams,
    control: &Control,
    log: &mut dyn FnMut(&str),
) -> Result<Value, String> {
    if client.identity.has_unconfigured_default_location() {
        return Err("请先填写所在城市与定位锚点".into());
    }
    let anchor = client.identity.anchor_coordinate()?;
    log("正在获取学校策略与实时点位…");
    let policy = api::policy::fetch_policy(client)?;
    let fences = match policy.geo_fence {
        Some(f) => f,
        None => api::fence::fetch_geo_fence_response(client)?,
    };
    let config = model::load_config();
    let fences = if !policy.run_areas.is_empty()
        || (params.route_mode == crate::track::generate_road::RouteMode::Road
            && !fences.polygons.is_empty())
    {
        api::campus::selected_response(
            fences,
            &policy.run_areas,
            config.school_campus.as_ref(),
            &client.login.as_ref().unwrap().unid,
        )?
    } else {
        fences
    };
    let area = (fences.area.run_area_id >= 0).then_some(fences.area.run_area_id);
    let points = api::points::fetch_points_ext(client, anchor, area, policy.points_distance, log)?;
    if points.is_empty() || api::points::points_bd(&points).len() != points.len() {
        return Err("学校未返回有效点位".into());
    }
    let route_points = api::points::route_points(&policy.must_points, &points);
    let mut track = if params.route_mode == crate::track::generate_road::RouteMode::Road {
        let graph = api::school_roads::load_routable_cancellable(
            &config.osm_path,
            &client.login.as_ref().unwrap().unid,
            &fences.polygons,
            &route_points,
            false,
            control,
            log,
        )?;
        match &params.terrain {
            Some(terrain) => crate::track::generate_road::build_road_simulated(
                params.dist,
                params.dur,
                params.seed,
                params.start_ms,
                &route_points,
                &route_points,
                &graph,
                terrain,
                control,
                log,
            )?,
            None => crate::track::generate_road::build_road(
                params.dist,
                params.dur,
                params.seed,
                params.start_ms,
                &route_points,
                &route_points,
                &graph,
            )?,
        }
    } else {
        match &params.terrain {
            Some(terrain) => crate::track::generator::build_simulated(
                params.dist,
                params.dur,
                params.seed,
                params.start_ms,
                &route_points,
                terrain,
                control,
                log,
            )?,
            None => crate::track::generator::build(
                params.dist,
                params.dur,
                params.seed,
                (anchor.latitude, anchor.longitude),
                params.start_ms,
                &route_points,
            ),
        }
    };
    if let Some(range) = params.manual_altitude_range {
        crate::track::altitude::override_bd_a_range(&mut track, range)?;
    } else if let Some(altitude) = params.manual_altitude {
        crate::track::altitude::override_bd_a(&mut track, altitude)?;
    }
    track.validate_consistency()?;
    Ok(
        json!({"distance":track.totalDistance,"duration":track.totalTime,"start_ms":params.start_ms,"seed":params.seed,
        "route":track.locations.iter().step_by(3).map(|p| json!([p.lat,p.lng])).collect::<Vec<_>>(),
        "points":points,"fences":fences.polygons}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_payloads_never_retain_nested_credentials() {
        assert_eq!(
            strip_credentials(
                json!({"name":"student","token":"private","nested":[{"access_token":"private","count":3}]})
            ),
            json!({"name":"student","nested":[{"count":3}]})
        );
    }
    #[test]
    fn invalid_requests_and_unknown_commands_fail_before_network() {
        assert!(dispatch(json!({"op":"oops"})).is_err());
        assert!(dispatch(json!({"op":"start","directory":"relative","command":"run"})).is_err());
        assert!(integer(&json!({"n": 2.5}), "n", 1, 1, 3).is_err());
        assert!(number(&json!({"n": -1}), "n", 1.0, 0.1, 10.0).is_err());
    }
    #[test]
    fn ffi_owns_response_and_rejects_invalid_json() {
        let input = CString::new("invalid").unwrap();
        let pointer = unsafe { neko_call(input.as_ptr()) };
        let response: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(pointer) }.to_str().unwrap()).unwrap();
        assert_eq!(response["ok"], false);
        unsafe {
            neko_free(pointer);
            neko_free(std::ptr::null_mut());
        }
    }
    #[test]
    fn job_bootstrap_and_cancellation_are_account_scoped() {
        let root = std::env::temp_dir().join(format!("dlpupass-neko-{}", uuid::Uuid::new_v4()));
        let response =
            dispatch(json!({"op":"start","directory":root,"command":"bootstrap"})).unwrap();
        let id = response["id"].as_str().unwrap();
        let result = loop {
            let poll = dispatch(json!({"op":"poll","id":id})).unwrap();
            if poll["done"] == true {
                break poll;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(result["result"]["ok"], true);
        assert!(!root.join("session.json").exists());
        assert!(root.join("identity.json").exists());
        dispatch(json!({"op":"forget","id":id})).unwrap();
        assert!(dispatch(json!({"op":"poll","id":id})).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
