//! CLI 子命令实现。

use super::{
    fmt_hms, get, get_int, get_int_in, get_pos_f64, jstr, logger, make_client, now_ms, parse_flags,
    parse_pace, password_from, print_rows, prompt_password, usage_error,
};
use crate::api::ai::AiMode;
use crate::api::client::ApiClient;
use crate::api::model;

/// 参数解析 / 校验失败时打印错误并以退出码 2 返回。
macro_rules! args {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return usage_error(&e),
        }
    };
}

pub fn dispatch(args: Vec<String>) -> i32 {
    let cmd = args.first().map(|x| x.as_str()).unwrap_or("help");
    let rest = args.iter().skip(1).map(|x| x.as_str()).collect::<Vec<_>>();
    // 无参数命令同样拒绝多余参数
    let no_args = || parse_flags(&rest, &[], &[]).map(|_| ());
    match cmd {
        "login" => cmd_login(&rest),
        "logout" => {
            args!(no_args());
            cmd_logout()
        }
        "run" => cmd_run(&rest),
        "template" => cmd_template(&rest),
        "simulate" => cmd_simulate(&rest),
        "ai" => cmd_ai(&rest),
        "ai-list" => {
            args!(no_args());
            cmd_ai_list()
        }
        "records" => {
            args!(no_args());
            cmd_records()
        }
        "records-raw" => cmd_records_raw(&rest),
        "record-info" => cmd_record_info(&rest),
        "obs-get" => cmd_obs_get(&rest),
        "obs-sample" => cmd_obs_sample(&rest),
        "ai-records" => cmd_ai_records(&rest),
        "ai-info" => cmd_ai_info(&rest),
        "semester" => {
            args!(no_args());
            cmd_semester()
        }
        "cheat" => cmd_cheat(&rest),
        "rank" => cmd_rank(&rest),
        "update" => cmd_update(&rest),
        "help" | "--help" | "-h" => {
            usage();
            0
        }
        _ => {
            eprintln!("未知命令: {cmd}");
            super::usage();
            super::EXIT_USAGE
        }
    }
}

fn usage() {
    super::usage();
}

fn cmd_login(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["user", "pass"], &["remember"]));
    let Some(user) = get(&flags, "user").filter(|u| !u.trim().is_empty()) else {
        return usage_error("缺少 --user");
    };
    if get(&flags, "pass").is_some() {
        eprintln!(
            "提示：--pass 会留在进程列表与 shell 历史中，建议改用环境变量 NSWT_PASS 或交互输入"
        );
    }
    let pw = match password_from(get(&flags, "pass"), std::env::var("NSWT_PASS").ok()) {
        Some(p) => p,
        None => match prompt_password() {
            Ok(p) => p,
            Err(e) => return usage_error(&e),
        },
    };
    let pw = pw.as_str();
    let identity = model::load_identity();
    let mut client = ApiClient::new(identity, None);
    let mut log = logger();
    match crate::api::login::login(&mut client, user, pw, &mut log) {
        Ok(s) => {
            if get(&flags, "remember").is_some() {
                let mut cfg = model::load_config();
                cfg.username = user.to_string();
                cfg.password = pw.to_string();
                cfg.remember = true;
                let _ = model::save_config(&cfg);
                println!("凭据已保存（会话失效时自动重登）");
            }
            println!("登录成功 uid={} unid={} name={}", s.uid, s.unid, s.name);
            0
        }
        Err(e) => {
            eprintln!("登录失败: {e}");
            1
        }
    }
}

fn cmd_logout() -> i32 {
    let identity = model::load_identity();
    let sess = model::load_session();
    let mut client = ApiClient::new(identity, sess.is_logged_in().then_some(sess));
    let mut log = logger();
    crate::api::login::logout(&mut client, &mut log);
    0
}

fn cmd_run(rest: &[&str]) -> i32 {
    use crate::start_time;
    let flags = args!(parse_flags(
        rest,
        &[
            "dist",
            "pace",
            "altitude",
            "ago",
            "days-ago",
            "time",
            "face",
            "seed",
            "route",
            "terrain-file",
            "simulation-config"
        ],
        &["terrain", "no-terrain"],
    ));
    // 参数先全部校验，再登录（避免参数写错还触发自动重登）
    let dist_km = args!(get_pos_f64(&flags, "dist"));
    let pace = match get(&flags, "pace") {
        Some(v) => Some(args!(parse_pace(v))),
        None => None,
    };
    let ago_min: Option<i64> = args!(get_int(&flags, "ago"));
    let days_ago = args!(get_int_in(&flags, "days-ago", 0, start_time::MAX_DAYS_AGO));
    let time_spec = match get(&flags, "time") {
        Some(v) => Some(args!(
            start_time::parse_hhmm(v).map_err(|e| format!("--time {e}"))
        )),
        None => None,
    };
    if time_spec.is_some() && ago_min.is_some() {
        return usage_error("--time 与 --ago 不能同时使用");
    }
    if days_ago.is_some() && time_spec.is_none() {
        return usage_error("--days-ago 需配合 --time HH:MM 使用");
    }
    let face = match get(&flags, "face") {
        None | Some("1") | Some("true") => true,
        Some("0") | Some("false") => false,
        Some(v) => return usage_error(&format!("--face 取值应为 0|1，收到 {v:?}")),
    };
    let (manual_altitude, manual_altitude_range) = match get(&flags, "altitude") {
        Some(value) => match crate::track::altitude::parse_spec(value) {
            Ok(Some(crate::track::altitude::AltitudeSpec::Single(value))) => (Some(value), None),
            Ok(Some(crate::track::altitude::AltitudeSpec::Range(range))) => (None, Some(range)),
            Ok(None) => (None, None),
            Err(e) => return usage_error(&format!("--altitude {e}")),
        },
        None => (None, None),
    };
    let seed: u64 = args!(get_int(&flags, "seed")).unwrap_or(0);
    let seed = if seed == 0 {
        (now_ms() % 2_147_483_647) as u64
    } else {
        seed
    };
    let route_flag = match get(&flags, "route") {
        None => None,
        Some(v @ ("legacy" | "road")) => Some(v),
        Some(v) => return usage_error(&format!("--route 取值应为 legacy|road，收到 {v:?}")),
    };

    let cfg = model::load_config();
    let terrain_settings = args!(run_terrain_settings(&flags, &cfg.terrain_simulation));
    args!(terrain_settings.validate_altitude(manual_altitude, manual_altitude_range));
    let terrain = args!(terrain_settings.load());
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let route_mode =
        crate::track::generate_road::RouteMode::from_str(route_flag.unwrap_or(&cfg.route_mode));

    let dist = match dist_km {
        Some(km) => km * 1000.0,
        None => (1.0 + rand::random::<f32>() * 0.5) as f64 * 1000.0,
    };
    let pace_s = pace.unwrap_or_else(|| 360.0 + rand::random::<f32>() * 120.0);
    let dur = (dist as f32 / 1000.0 * pace_s) as i64;
    // 读一次真实时钟，三种开始时间规则都基于同一时刻
    let now = chrono::Local::now();
    let start = if let Some((h, m)) = time_spec {
        // --time "HH:MM" 配合 --days-ago（0-3）
        args!(start_time::specified(
            &now,
            days_ago.unwrap_or(0),
            h,
            m,
            dur
        ))
    } else if let Some(ago) = ago_min {
        args!(start_time::ago(now.timestamp_millis(), ago, dur).map_err(|e| format!("--ago {e}")))
    } else {
        start_time::random_ago(now.timestamp_millis(), dur, rand::random::<f64>())
    };
    if start.clamped && (time_spec.is_some() || ago_min.is_some()) {
        println!(
            "提示：指定的开始时间会使结束时刻晚于当前时间，已提前到 {}",
            fmt_hms(start.ms)
        );
    }
    let start_ms = start.ms;

    println!(
        "参数：{:.0}m / {}s / 配速 {}:{:02}/km / 开始 {}",
        dist,
        dur,
        pace_s as i64 / 60,
        pace_s as i64 % 60,
        fmt_hms(start_ms)
    );

    let mut log = logger();
    let params = crate::api::flow::RunParams {
        dist,
        dur,
        start_ms,
        face_check: face as i64,
        manual_altitude,
        manual_altitude_range,
        seed,
        route_mode,
        terrain,
    };
    match crate::api::flow::run_full_flow(&mut client, &params, &mut log) {
        Ok(out) => {
            if let Some(simulation) = &out.simulation {
                println!(
                    "山地仿真：{} 个观测样本已保留，位置/配速/步数通过原有协议提交",
                    simulation.sensors.len()
                );
            }
            println!(
                "记录提交成功 rrid={} uuid={}；OBS 上传={}/2；详情读取={}；服务端达标判定={}",
                out.result.rrid,
                out.result.uuid,
                out.obs_ok,
                if out.verification.detail.is_some() {
                    "成功"
                } else {
                    "失败"
                },
                out.verification.label()
            );
            0
        }
        Err(e) => {
            eprintln!("跑步提交失败: {e}");
            1
        }
    }
}

fn run_terrain_settings(
    flags: &super::Flags,
    saved: &crate::track::simulation_adapter::TerrainSimulationSettings,
) -> Result<crate::track::simulation_adapter::TerrainSimulationSettings, String> {
    let on = get(flags, "terrain").is_some();
    let off = get(flags, "no-terrain").is_some();
    let file = get(flags, "terrain-file");
    let config = get(flags, "simulation-config");
    if off && (on || file.is_some() || config.is_some()) {
        return Err(
            "--no-terrain 不能与 --terrain、--terrain-file 或 --simulation-config 同时使用".into(),
        );
    }
    let mut settings = saved.clone();
    if off {
        settings.enabled = false;
    }
    if on || file.is_some() || config.is_some() {
        settings.enabled = true;
    }
    if let Some(path) = file {
        settings.terrain_path = path.into();
    }
    if let Some(path) = config {
        settings.config_path = path.into();
    }
    Ok(settings)
}

fn cmd_simulate(rest: &[&str]) -> i32 {
    use crate::simulation::{simulate, RouteInput, SimulationConfig};
    use crate::terrain::TerrainGrid;
    use std::io::Write;
    let flags = args!(parse_flags(
        rest,
        &["route", "terrain", "config", "output", "speed", "duration", "seed"],
        &["sensors"],
    ));
    let Some(route_path) = get(&flags, "route").filter(|s| !s.trim().is_empty()) else {
        return usage_error("simulate 缺少 --route 路线 JSON");
    };
    let Some(terrain_path) = get(&flags, "terrain").filter(|s| !s.trim().is_empty()) else {
        return usage_error("simulate 缺少 --terrain 高程网格 JSON");
    };
    let route_bytes = args!(std::fs::read(route_path).map_err(|e| format!("读取路线失败: {e}")));
    let route: RouteInput =
        args!(serde_json::from_slice(&route_bytes).map_err(|e| format!("路线格式错误: {e}")));
    let terrain = args!(TerrainGrid::load(terrain_path));
    let mut config: SimulationConfig = if let Some(path) = get(&flags, "config") {
        let bytes = args!(std::fs::read(path).map_err(|e| format!("读取仿真配置失败: {e}")));
        args!(serde_json::from_slice(&bytes).map_err(|e| format!("仿真配置格式错误: {e}")))
    } else {
        SimulationConfig::default()
    };
    if let Some(speed) = args!(get_pos_f64(&flags, "speed")) {
        config.base_speed_mps = speed;
    }
    if let Some(duration) = args!(get_pos_f64(&flags, "duration")) {
        config.target_duration_s = Some(duration);
    }
    if let Some(seed) = args!(get_int(&flags, "seed")) {
        config.seed = seed;
    }
    if get(&flags, "sensors").is_some() {
        config.sensors = true;
    }
    let output = match simulate(&route, &terrain, &config) {
        Ok(output) => output,
        Err(e) => {
            eprintln!("本地仿真失败: {e}");
            return 1;
        }
    };
    if let Some(path) = get(&flags, "output") {
        // Never truncate an existing file, including the route and raster inputs.
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => file,
            Err(e) => {
                eprintln!("创建输出失败（不覆盖已有文件）: {e}");
                return 1;
            }
        };
        let mut writer = std::io::BufWriter::new(file);
        if let Err(e) = serde_json::to_writer(&mut writer, &output)
            .map_err(|e| e.to_string())
            .and_then(|_| writer.flush().map_err(|e| e.to_string()))
        {
            eprintln!("写入仿真结果失败，{path} 可能不完整: {e}");
            return 1;
        }
        println!("结果已保存到 {path}");
    }
    println!(
        "本地仿真：{:.1}m / {:.3}s，爬升 {:.2}m，下降 {:.2}m，运动点 {}，传感器样本 {}",
        output.distance_m,
        output.duration_s,
        output.ascent_m,
        output.descent_m,
        output.motion.len(),
        output.sensors.len()
    );
    println!(
        "一致性校验通过；约束下最短时长 {:.3}s；最大爬升率 {:.3}m/s",
        output.fastest_duration_s, output.validation.max_climb_mps
    );
    for warning in &output.warnings {
        println!("提示：{warning}");
    }
    0
}

fn cmd_template(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["file"], &[]));
    let Some(path) = get(&flags, "file") else {
        return usage_error("缺少 --file <GPX/JSON>");
    };
    match crate::template::load(path) {
        Ok(samples) => {
            let summary = crate::template::summarize(path, &samples);
            println!("本地模板分析（仅预览，不上传）");
            println!("文件：{}", summary.source);
            println!("采样点：{}", summary.samples);
            println!("海拔范围：{:.1}–{:.1} m", summary.min_m, summary.max_m);
            println!(
                "累计上升：{:.1} m；累计下降：{:.1} m",
                summary.gain_m, summary.loss_m
            );
            0
        }
        Err(e) => {
            eprintln!("模板读取失败：{e}");
            1
        }
    }
}

fn cmd_ai(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(
        rest,
        &["sport", "mode", "score", "count", "minutes"],
        &[]
    ));
    let Some(sport) = args!(get_int(&flags, "sport")) else {
        return usage_error("缺少 --sport");
    };
    // 按分钟（--minutes 1-30）或按次（--count 5-1000 步长 5）；--score 两种模式通用
    let score_or = |alt: &str| -> Result<Option<i64>, String> {
        match get_int::<i64>(&flags, "score")? {
            Some(v) => Ok(Some(v)),
            None => get_int::<i64>(&flags, alt),
        }
    };
    let mode = match get(&flags, "mode").unwrap_or("min") {
        "count" => {
            let reps = args!(score_or("count")).unwrap_or(5).clamp(5, 1000);
            AiMode::Count {
                reps: (reps / 5) * 5,
            }
        }
        "min" => {
            let minutes = args!(score_or("minutes")).unwrap_or(1).clamp(1, 30);
            AiMode::Minutes { minutes }
        }
        other => return usage_error(&format!("--mode 取值应为 min|count，收到 {other:?}")),
    };
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut log = logger();
    match crate::api::flow::run_ai_submit(&mut client, sport, mode, &mut log) {
        Ok(biz) => {
            println!(
                "AI 提交成功 服务器={} {}",
                biz.get("error").and_then(|e| e.as_i64()).unwrap_or(0),
                biz.get("message").and_then(|m| m.as_str()).unwrap_or("")
            );
            0
        }
        Err(e) => {
            eprintln!("AI 提交失败: {e}");
            1
        }
    }
}

fn cmd_ai_list() -> i32 {
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut log = logger();
    match crate::api::flow::run_ai_list(&mut client, &mut log) {
        Ok(list) => {
            for s in list {
                println!("id={:<4} {}", s.id, s.name);
            }
            0
        }
        Err(e) => {
            eprintln!("拉取失败: {e}");
            1
        }
    }
}

fn cmd_records() -> i32 {
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut log = logger();
    match crate::api::flow::run_records(&mut client, &mut log) {
        Ok(rows) => {
            print_rows(&rows);
            0
        }
        Err(e) => {
            eprintln!("拉取失败: {e}");
            1
        }
    }
}

/// 记录列表原始探测：支持自定义 body，打印条数与日期范围。
fn cmd_records_raw(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["body", "out"], &[]));
    let body = get(&flags, "body").unwrap_or("{}").to_string();
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match client.call("POST", crate::api::records::RECORDS_PATH, &body, &[]) {
        Ok(biz) => {
            let data = crate::api::client::parse_data_field(&biz);
            let arr = data.as_array().cloned().unwrap_or_default();
            let n = arr.len();
            println!("条数: {n}");
            if let Some(out) = get(&flags, "out") {
                std::fs::write(out, serde_json::to_string_pretty(&arr).unwrap_or_default())
                    .map_err(|e| eprintln!("写入失败: {e}"))
                    .ok();
                println!("已保存 {out}");
            } else if n > 0 {
                // 按字符截断：JSON 含中文时按字节切片会 panic
                use crate::textlog::truncate;
                println!("首条: {}", truncate(&arr[0].to_string(), 300));
                println!("末条: {}", truncate(&arr[n - 1].to_string(), 300));
            } else {
                println!("data: {}", crate::textlog::truncate(&data.to_string(), 500));
            }
            0
        }
        Err(e) => {
            eprintln!("拉取失败: {e}");
            1
        }
    }
}

/// 拉取 OBS 对象（签名 GET），解码 gzip+base64 字段后保存。
fn cmd_obs_get(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["key", "out"], &[]));
    let Some(key) = get(&flags, "key") else {
        return usage_error("缺少 --key");
    };
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut log = |s: &str| eprintln!("{s}");
    match crate::api::obs::fetch_object(&mut client, key, &mut log) {
        Ok(v) => {
            let out = get(&flags, "out").unwrap_or("obs_real.json");
            // 解码 gzip+base64 值便于直接阅读
            let decoded = decode_gz_fields(&v);
            std::fs::write(
                out,
                serde_json::to_string_pretty(&decoded).unwrap_or_default(),
            )
            .map_err(|e| eprintln!("写入失败: {e}"))
            .ok();
            println!("已保存 {out}");
            0
        }
        Err(e) => {
            eprintln!("拉取失败: {e}");
            1
        }
    }
}

/// 解码 OBS 对象中 gzip+base64 的字段值（尽力而为）。
fn decode_gz_fields(v: &serde_json::Value) -> serde_json::Value {
    use base64::Engine;
    let mut out = v.clone();
    if let Some(obj) = out.as_object_mut() {
        for (_k, val) in obj.iter_mut() {
            if let Some(s) = val.as_str() {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(s)
                    .ok()
                    .and_then(|raw| {
                        let mut dec = flate2::read::GzDecoder::new(raw.as_slice());
                        use std::io::Read;
                        let mut txt = String::new();
                        dec.read_to_string(&mut txt).ok()?;
                        Some(txt)
                    });
                if let Some(txt) = decoded {
                    let parsed = serde_json::from_str::<serde_json::Value>(&txt)
                        .map(|x| x.to_string())
                        .unwrap_or(txt);
                    *val = serde_json::Value::String(parsed);
                }
            }
        }
    }
    out
}

/// 本地生成一份 OBS 对象样本（不提交；结构对照用）。
fn cmd_obs_sample(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(
        rest,
        &["dist", "dur", "rrid", "seed", "out"],
        &[]
    ));
    let dist = args!(get_pos_f64(&flags, "dist")).unwrap_or(1050.0);
    let dur = args!(get_int_in(&flags, "dur", 1, 86_400)).unwrap_or(480);
    let rrid = args!(get_int::<i64>(&flags, "rrid")).unwrap_or(1320000000);
    let seed = args!(get_int::<u64>(&flags, "seed")).unwrap_or(7);
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut log = |s: &str| eprintln!("{s}");
    let anchor = match client.identity.anchor_coordinate() {
        Ok(value) => value,
        Err(e) => {
            eprintln!("定位锚点无效: {e}");
            return 1;
        }
    };
    let pts = match crate::api::points::fetch_points(&mut client, anchor, &mut log) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("点位拉取失败: {e}");
            return 1;
        }
    };
    let pts_bd = crate::api::points::points_bd(&pts);
    let start_ms = crate::crypto::envelope::now_ms() - dur * 1000;
    let track = match crate::track::generator::try_build(
        dist,
        dur,
        seed,
        (anchor.latitude, anchor.longitude),
        start_ms,
        &pts_bd,
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("轨迹生成失败: {e}");
            return 1;
        }
    };
    let sess = client.login.clone().unwrap_or_default();
    let uuid = uuid::Uuid::new_v4().to_string().to_uppercase();
    let obj = crate::track::wire::build_obs_object(
        &track,
        rrid,
        &uuid,
        sess.uid,
        &crate::track::wire::five_point_wrapper(&pts, &track, &Default::default()),
    );
    let out = get(&flags, "out").unwrap_or("obs_ours.json");
    let decoded = decode_gz_fields(&obj);
    std::fs::write(
        out,
        serde_json::to_string_pretty(&decoded).unwrap_or_default(),
    )
    .map_err(|e| eprintln!("写入失败: {e}"))
    .ok();
    println!("已保存 {out}");
    0
}

/// 单条跑步记录原始详情。
fn cmd_record_info(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["rrid"], &[]));
    let Some(rrid) = args!(get_int::<i64>(&flags, "rrid")) else {
        return usage_error("缺少 --rrid");
    };
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match crate::api::records::fetch_one_record(&mut client, rrid) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(e) => {
            eprintln!("拉取失败: {e}");
            return 1;
        }
    }
    0
}

/// 单条 AI 记录全量详情。
fn cmd_ai_info(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["id"], &[]));
    let Some(id) = args!(get_int::<i64>(&flags, "id")) else {
        return usage_error("缺少 --id");
    };
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match crate::api::ai::fetch_record_detail(&mut client, id) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(e) => {
            eprintln!("拉取失败: {e}");
            return 1;
        }
    }
    0
}

fn cmd_ai_records(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["sport"], &[]));
    let Some(sport) = args!(get_int(&flags, "sport")) else {
        return usage_error("缺少 --sport");
    };
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match crate::api::ai::fetch_records(&mut client, sport, 50) {
        Ok(page) => {
            use chrono::TimeZone;
            for g in page.groups {
                let date = chrono::Local
                    .timestamp_millis_opt(g.score_date)
                    .single()
                    .map(|t| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                println!("{date} ×{}：", g.frequency);
                for r in g.records {
                    let grade = if r.rtype == 2 {
                        format!("{:.1} 秒", r.score.parse::<f64>().unwrap_or(0.0) / 1000.0)
                    } else {
                        format!("{} 个", r.score)
                    };
                    let finish = chrono::Local
                        .timestamp_millis_opt(r.score_date)
                        .single()
                        .map(|t| t.format("%H:%M:%S").to_string())
                        .unwrap_or_default();
                    let video = if r.has_video { "有" } else { "-" };
                    println!("  {} {} 完成 {finish} 视频 {video}", r.name, grade);
                }
            }
            0
        }
        Err(e) => {
            eprintln!("拉取失败: {e}");
            1
        }
    }
}

fn cmd_semester() -> i32 {
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut log = logger();
    match crate::api::semester::query(&mut client, &mut log) {
        Ok(r) => {
            if let Some(s) = r.summary {
                println!(
                    "学期：{}  有效次数：{}/{}  有效里程：{:.2} km（总 {:.2} km）",
                    s.sname,
                    s.semester_valid_count,
                    s.semester_count,
                    s.semester_valid_dis / 1000.0,
                    s.semester_dis / 1000.0
                );
            }
            if !r.personal_raw.is_null() {
                println!("个人完成度：{}", r.personal_raw);
            }
            0
        }
        Err(e) => {
            eprintln!("拉取失败: {e}");
            1
        }
    }
}

fn cmd_cheat(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &["page"], &[]));
    let page: i64 = args!(get_int_in(&flags, "page", 1, i64::MAX)).unwrap_or(1);
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut log = logger();
    match crate::api::cheat::query(&mut client, page, &mut log) {
        Ok(r) => {
            println!("自查：{}", r.label());
            println!(
                "检查时间：{}",
                crate::track::geom::fmt_cst(r.checked_at_ms, "%Y-%m-%d %H:%M:%S 北京时间")
            );
            if r.status() == crate::api::cheat::Status::Flagged {
                println!("服务端原因：{}；发生时间：{}", r.reason(), r.occurred_at());
                if let Some(id) = r.record_id() {
                    println!("关联跑步记录：{id}");
                }
            }
            println!("本页通报 {} 条（非全校总数）", r.list.len());
            for item in r.list.iter().take(20) {
                println!(
                    "  {} | {} | {}",
                    jstr(item, &["name", "userName"]),
                    jstr(item, &["reason", "punishReason", "cause"]),
                    jstr(item, &["createTime", "time", "date"]),
                );
            }
            0
        }
        Err(e) => {
            eprintln!("检查失败: {e}");
            1
        }
    }
}

fn cmd_rank(rest: &[&str]) -> i32 {
    let query = args!(parse_rank(rest));
    let mut client = match make_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let res = match query {
        RankQuery::Indoor { range, gender } => {
            crate::api::rank::indoor_rank(&mut client, range, gender)
        }
        RankQuery::History { sort, gender } => {
            crate::api::rank::history_rank(&mut client, sort, gender)
        }
        RankQuery::Main {
            rtype,
            sort,
            gender,
            date,
        } => crate::api::rank::main_rank(&mut client, rtype, sort, gender, date),
    };
    match res {
        Ok(rows) => {
            for r in rows {
                println!("{:<4} {}  {:.2} km", r.sort, r.name, r.length / 1000.0);
            }
            0
        }
        Err(e) => {
            eprintln!("查询失败: {e}");
            1
        }
    }
}

#[derive(Debug, PartialEq)]
enum RankQuery {
    Main {
        rtype: i64,
        sort: i64,
        gender: Option<i64>,
        date: Option<String>,
    },
    /// gender 缺省 -1（不限）。
    Indoor {
        range: i64,
        gender: i64,
    },
    History {
        sort: i64,
        gender: i64,
    },
}

/// rank 参数：首个参数为 main|indoor|history 时作为榜单类型；缺省或以 `-` 开头
/// 时为 main 且全部参数参与解析（`rank --type 2` 不再丢参数）。
fn parse_rank(rest: &[&str]) -> Result<RankQuery, String> {
    let (kind, rest) = match rest.split_first() {
        Some((k, tail)) if !k.starts_with('-') => (*k, tail),
        _ => ("main", rest),
    };
    match kind {
        "main" => {
            let f = parse_flags(rest, &["type", "sort", "gender", "date"], &[])?;
            let date = match get(&f, "date") {
                Some(d) => {
                    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                        .map_err(|_| format!("--date 格式应为 YYYY-MM-DD，收到 {d:?}"))?;
                    Some(d.to_string())
                }
                None => None,
            };
            Ok(RankQuery::Main {
                rtype: get_int_in(&f, "type", 1, 3)?.unwrap_or(1),
                sort: get_int_in(&f, "sort", 1, 2)?.unwrap_or(1),
                gender: get_int_in(&f, "gender", 0, 1)?,
                date,
            })
        }
        "indoor" => {
            let f = parse_flags(rest, &["range", "gender"], &[])?;
            Ok(RankQuery::Indoor {
                range: get_int_in(&f, "range", 1, 3)?.unwrap_or(1),
                gender: get_int_in(&f, "gender", 0, 1)?.unwrap_or(-1),
            })
        }
        "history" => {
            let f = parse_flags(rest, &["sort", "gender"], &[])?;
            Ok(RankQuery::History {
                sort: get_int_in(&f, "sort", 1, 2)?.unwrap_or(1),
                gender: get_int_in(&f, "gender", 0, 1)?.unwrap_or(-1),
            })
        }
        other => Err(format!(
            "未知榜单类型 {other:?}（可用：main|indoor|history）"
        )),
    }
}

fn cmd_update(rest: &[&str]) -> i32 {
    let flags = args!(parse_flags(rest, &[], &["check"]));
    let check_only = get(&flags, "check").is_some();
    crate::update::cleanup_residue();
    let mut log = logger();
    let release = match crate::update::check_latest(&mut log) {
        Ok(r) => r,
        Err(e) => {
            println!("× 检查更新失败: {e}");
            return 1;
        }
    };
    let Some(rel) = release else {
        println!("√ 已是最新版本（v{}）", crate::update::current_version());
        return 0;
    };
    println!(
        "发现新版本 {}（当前 v{}，{:.1} MB）",
        rel.tag,
        crate::update::current_version(),
        rel.asset_size as f64 / 1024.0 / 1024.0
    );
    if !rel.notes.is_empty() {
        for line in rel.notes.lines().take(8) {
            println!("  {line}");
        }
    }
    if check_only {
        println!("仅检查（--check），未下载。下载地址：{}", rel.asset_url);
        return 0;
    }
    #[cfg(target_os = "android")]
    {
        println!("Android 版请在界面（关于页）中下载并安装更新");
        0
    }
    #[cfg(not(target_os = "android"))]
    {
        println!("开始下载 {}…", rel.asset_name);
        let mut last_pct = u64::MAX;
        let bytes = match crate::update::download_verified(&rel, |done, total| {
            if let Some(t) = total.filter(|t| *t > 0) {
                let pct = done * 100 / t;
                if pct != last_pct && pct % 5 == 0 {
                    println!("  {pct}%");
                    last_pct = pct;
                }
            }
        }) {
            Ok(b) => b,
            Err(e) => {
                println!("× 下载失败: {e}");
                return 1;
            }
        };
        println!("下载完成（{} 字节），解包替换…", bytes.len());
        let bin = match crate::update::extract(&rel.asset_name, &bytes) {
            Ok(b) => b,
            Err(e) => {
                println!("× {e}");
                return 1;
            }
        };
        match crate::update::apply(&bin) {
            Ok(()) => {
                println!("√ 已更新到 {}，请重新运行命令", rel.tag);
                0
            }
            Err(e) => {
                println!("× 替换失败: {e}");
                1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_cli_overrides_do_not_change_legacy_defaults() {
        use crate::track::simulation_adapter::TerrainSimulationSettings;
        let parse = |args: &[&str]| {
            parse_flags(
                args,
                &["terrain-file", "simulation-config"],
                &["terrain", "no-terrain"],
            )
            .unwrap()
        };
        let saved = TerrainSimulationSettings::default();
        assert!(!run_terrain_settings(&parse(&[]), &saved).unwrap().enabled);
        assert!(
            run_terrain_settings(&parse(&["--terrain-file", "grid.json"]), &saved)
                .unwrap()
                .enabled
        );
        let enabled = TerrainSimulationSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(
            !run_terrain_settings(&parse(&["--no-terrain"]), &enabled)
                .unwrap()
                .enabled
        );
        assert!(run_terrain_settings(&parse(&["--terrain", "--no-terrain"]), &saved).is_err());
    }

    #[test]
    fn local_simulation_exports_and_never_overwrites_existing_output() {
        let dir = std::env::temp_dir().join(format!("neko-simulation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let output = dir.join("result.json");
        let route = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/terrain/route.json");
        let terrain = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/terrain/grid.json");
        let arguments = [
            "simulate",
            "--route",
            route,
            "--terrain",
            terrain,
            "--output",
            output.to_str().unwrap(),
        ];
        let run = || dispatch(arguments.iter().map(|s| s.to_string()).collect());
        assert_eq!(run(), 0);
        let original = std::fs::read(&output).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(value["validation"]["passed"], true);
        assert_eq!(run(), 1);
        assert_eq!(std::fs::read(&output).unwrap(), original);
        assert_eq!(
            dispatch(vec!["simulate".into(), "--route".into(), route.into()]),
            2
        );
        std::fs::remove_file(&output).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn rank_kind_defaults_to_main_without_eating_flags() {
        assert_eq!(
            parse_rank(&["--type", "2", "--sort", "2"]).unwrap(),
            RankQuery::Main {
                rtype: 2,
                sort: 2,
                gender: None,
                date: None
            }
        );
        assert_eq!(
            parse_rank(&[]).unwrap(),
            RankQuery::Main {
                rtype: 1,
                sort: 1,
                gender: None,
                date: None
            }
        );
        assert_eq!(
            parse_rank(&["main", "--type=3", "--gender", "0", "--date", "2026-10-01"]).unwrap(),
            RankQuery::Main {
                rtype: 3,
                sort: 1,
                gender: Some(0),
                date: Some("2026-10-01".into())
            }
        );
    }

    #[test]
    fn rank_gender_reaches_indoor_and_history() {
        assert_eq!(
            parse_rank(&["indoor", "--range", "2", "--gender", "1"]).unwrap(),
            RankQuery::Indoor {
                range: 2,
                gender: 1
            }
        );
        assert_eq!(
            parse_rank(&["indoor"]).unwrap(),
            RankQuery::Indoor {
                range: 1,
                gender: -1
            }
        );
        assert_eq!(
            parse_rank(&["history", "--sort", "2", "--gender", "0"]).unwrap(),
            RankQuery::History { sort: 2, gender: 0 }
        );
    }

    #[test]
    fn rank_rejects_bad_input() {
        assert!(parse_rank(&["weekly"]).is_err());
        assert!(parse_rank(&["--type", "abc"]).is_err());
        assert!(parse_rank(&["--type", "4"]).is_err());
        assert!(parse_rank(&["indoor", "--type", "1"]).is_err());
        assert!(parse_rank(&["--date", "2026/10/01"]).is_err());
        assert!(parse_rank(&["history", "--gender", "2"]).is_err());
    }

    #[test]
    fn usage_errors_exit_2_before_network() {
        // 这些都在登录 / 联网之前因参数错误返回
        let run = |v: &[&str]| dispatch(v.iter().map(|s| s.to_string()).collect());
        assert_eq!(run(&["run", "--dist", "abc"]), 2);
        assert_eq!(run(&["run", "--bogus", "1"]), 2);
        assert_eq!(run(&["run", "--time", "7:75"]), 2);
        assert_eq!(run(&["run", "--time", "07:00", "--ago", "10"]), 2);
        assert_eq!(run(&["run", "--days-ago", "1"]), 2);
        assert_eq!(run(&["run", "--face", "maybe"]), 2);
        assert_eq!(run(&["run", "--route", "fly"]), 2);
        assert_eq!(run(&["records", "--x"]), 2);
        assert_eq!(run(&["rank", "--sort", "x"]), 2);
        assert_eq!(run(&["update", "--check=1"]), 2);
        assert_eq!(run(&["ai", "--sport", "1", "--mode", "hours"]), 2);
        assert_eq!(run(&["login", "--pass", "-x"]), 2);
        assert_eq!(run(&["no-such-command"]), 2);
    }
}
