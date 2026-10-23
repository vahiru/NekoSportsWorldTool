//! 命令行入口：子命令分发 + 共享助手。
//! 子命令实现见 cmds.rs；数据文件与 GUI 共用（exe 同目录）。
//!
//! 青龙定时任务：首次 `login --user .. --pass .. --remember`，之后定时 `run`
//! 即可（会话失效且已记住凭据时自动重登）。

mod cmds;

use crate::api::client::ApiClient;
use crate::api::model::{self, Session};
use crate::api::records::RecordRow;

pub fn main(args: Vec<String>) -> i32 {
    cmds::dispatch(args)
}

fn usage() {
    println!(
        r#"NekoSportsWorldTool CLI（full 构建无参数启动 GUI）

  login   --user <手机号> [--pass <密码>] [--remember]
                                           登录并保存会话；--remember 同时保存凭据供自动重登
                                           密码优先级：--pass > 环境变量 NSWT_PASS > 终端交互输入
  logout                                   登出并清理本地会话
  run    [--dist km] [--pace 秒/km] [--altitude 米或min-max] [--ago 分钟] [--days-ago 0-3 --time HH:MM] [--face 0|1] [--seed n] [--route legacy|road]
                                           跑步全链：策略-点位-轨迹-提交-OBS-验证（--route 选择路线算法）
         [--terrain | --no-terrain] [--terrain-file 高程JSON] [--simulation-config 配置JSON]
                                           可选山地运动学；高程文件参数也会启用；与手动海拔互斥
  template --file <GPX/JSON>                本地读取真实记录，分析海拔（不会上传）
  simulate --route <路线JSON> --terrain <高程JSON> [--config 配置JSON] [--speed m/s] [--duration 秒] [--seed n] [--sensors] [--output 新文件JSON]
                                           本地山地轨迹仿真与一致性校验；不登录、不上传
  ai-list                                  AI 运动项目列表
  ai     --sport <id> [--mode min|count] [--score n]
                                           AI 运动：min 按分钟 1-30；count 按次 5-1000 步长 5
  records                                  跑步记录列表
  ai-records --sport <id>                  AI 运动记录（按天分组）
  semester                                 学期完成度
  cheat  [--page n]                        违规自查
  rank   [main] --type 1|2|3 --sort 1|2 [--gender 0|1] [--date YYYY-MM-DD]
                                           排行榜（1个人 2班级 3院系；1日 2月）
  rank   indoor --range 1|2|3 [--gender 0|1]
                                           室内榜（1日 2周 3月）
  rank   history --sort 1|2 [--gender 0|1] 历史榜
  update [--check]                        检查更新；默认下载并自替换（--check 仅检查）

参数均支持 `--name value` 与 `--name=value`；未知参数或无法解析的取值会报错（退出码 2）。
当天榜单通常在有效里程产生后才有数据；run 默认随机 1.0~1.5 km / 6~8 分配速 / 30-300 分钟前；
开始时间（含 --time / --ago）会保证「开始 + 用时」不晚于当前时刻，必要时自动提前。"#
    );
}

/// 参数错误退出码（区别于业务失败的 1）。
pub(crate) const EXIT_USAGE: i32 = 2;

/// 已解析的 `--name value` 列表（同名参数后者覆盖前者）。
pub(crate) type Flags = Vec<(String, String)>;

/// 按命令声明解析参数：
/// - `value` 中的参数总是取下一个参数作为值（即使以 `-` 开头，如密码 `-Abc123`），
///   也支持 `--name=value`；
/// - `switches` 中的开关不带值（`--name=..` 报错）；
/// - 未声明的参数、多余的位置参数一律报错（调用方以退出码 2 结束）。
pub(crate) fn parse_flags(
    rest: &[&str],
    value: &[&str],
    switches: &[&str],
) -> Result<Flags, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i];
        i += 1;
        let body = arg
            .strip_prefix("--")
            .or_else(|| arg.strip_prefix('-'))
            .filter(|b| !b.is_empty() && !b.starts_with('-'))
            .ok_or_else(|| format!("无法识别的参数 {arg:?}（参数需以 -- 开头）"))?;
        let (name, inline) = match body.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (body, None),
        };
        if value.contains(&name) {
            let v = match inline {
                Some(v) => v,
                None => {
                    let v = rest.get(i).ok_or_else(|| format!("--{name} 缺少参数值"))?;
                    i += 1;
                    v
                }
            };
            out.push((name.to_string(), v.to_string()));
        } else if switches.contains(&name) {
            if inline.is_some() {
                return Err(format!("--{name} 是开关参数，不接受取值"));
            }
            out.push((name.to_string(), "1".into()));
        } else {
            let all: Vec<String> = value
                .iter()
                .chain(switches)
                .map(|n| format!("--{n}"))
                .collect();
            return Err(if all.is_empty() {
                format!("未知参数 --{name}（该命令不接受参数）")
            } else {
                format!("未知参数 --{name}（可用：{}）", all.join(" "))
            });
        }
    }
    Ok(out)
}

pub(crate) fn get<'a>(flags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// 取整数参数：缺省 Ok(None)，无法解析报错（不再静默回退默认值）。
pub(crate) fn get_int<T: std::str::FromStr>(
    flags: &[(String, String)],
    name: &str,
) -> Result<Option<T>, String> {
    match get(flags, name) {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|_| format!("--{name} 需要整数，收到 {v:?}")),
    }
}

/// 取整数参数并校验范围（含端点）。
pub(crate) fn get_int_in(
    flags: &[(String, String)],
    name: &str,
    lo: i64,
    hi: i64,
) -> Result<Option<i64>, String> {
    match get_int::<i64>(flags, name)? {
        Some(v) if !(lo..=hi).contains(&v) => Err(format!("--{name} 取值应为 {lo}-{hi}，收到 {v}")),
        v => Ok(v),
    }
}

/// 取正的有限小数参数。
pub(crate) fn get_pos_f64(flags: &[(String, String)], name: &str) -> Result<Option<f64>, String> {
    match get(flags, name) {
        None => Ok(None),
        Some(v) => match v.trim().parse::<f64>() {
            Ok(x) if x.is_finite() && x > 0.0 => Ok(Some(x)),
            _ => Err(format!("--{name} 需要大于 0 的数字，收到 {v:?}")),
        },
    }
}

/// 参数错误：打印并返回退出码 2。
pub(crate) fn usage_error(e: &str) -> i32 {
    eprintln!("参数错误: {e}（用法见 help）");
    EXIT_USAGE
}

/// 密码来源优先级：--pass > 环境变量 NSWT_PASS（空值视为未提供）。
pub(crate) fn password_from(flag: Option<&str>, env: Option<String>) -> Option<String> {
    flag.map(str::to_string)
        .or_else(|| env.filter(|v| !v.is_empty()))
}

/// 交互式读取密码（仅 stdin 为终端时）；Unix 下借助 stty 关闭回显。
pub(crate) fn prompt_password() -> Result<String, String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err("缺少密码：请使用 --pass、环境变量 NSWT_PASS，或在终端中交互输入".into());
    }
    eprint!("密码（输入不回显）: ");
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    let echo_off = std::process::Command::new("stty")
        .arg("-echo")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    #[cfg(unix)]
    if echo_off {
        let _ = std::process::Command::new("stty").arg("echo").status();
    }
    eprintln!();
    read.map_err(|e| format!("读取密码失败: {e}"))?;
    let pw = line.trim_end_matches(['\r', '\n']).to_string();
    if pw.is_empty() {
        return Err("密码不能为空".into());
    }
    Ok(pw)
}

pub(crate) fn logger() -> impl FnMut(&str) {
    |s: &str| println!("{}", crate::textlog::clean(s))
}

pub(crate) fn silent() -> impl FnMut(&str) {
    |_s: &str| {}
}

/// 自动登录（GUI 记住密码 / login --remember 落盘的凭据）。
pub(crate) fn auto_login(identity: &model::HeaderIdentity) -> Result<Session, String> {
    let cfg = model::load_config();
    if cfg.remember && !cfg.username.is_empty() && !cfg.password.is_empty() {
        println!("[login] 使用已保存的账号自动登录…");
        let mut client = ApiClient::new(identity.clone(), None);
        return crate::api::login::login(&mut client, &cfg.username, &cfg.password, &mut silent());
    }
    Err("未登录：先执行 login 子命令，或在 GUI 勾选记住密码".into())
}

/// 构造已登录客户端；本地会话失效时自动重登一次。
pub(crate) fn make_client() -> Result<ApiClient, String> {
    let identity = model::load_identity();
    let client = {
        let sess = model::load_session();
        if sess.is_logged_in() {
            ApiClient::new(identity, Some(sess))
        } else {
            let s = auto_login(&identity)?;
            ApiClient::new(identity, Some(s))
        }
    };
    // 不做前置健康检查：业务错误（如新账号无学期数据）不等于会话过期，
    // 贸然重登会把好 session 删掉并触发 10121。会话真正过期时业务接口
    // 自然返回 401，由调用方处理。
    Ok(client)
}

pub(crate) fn now_ms() -> i64 {
    crate::crypto::envelope::now_ms()
}

pub(crate) fn fmt_hms(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

pub(crate) fn print_rows(rows: &[RecordRow]) {
    println!(
        "{:<16} {:>8} {:>8} {:>6} {:>6} {:>4} rrid",
        "时间", "距离m", "时长", "配速", "步频", "达标"
    );
    for r in rows {
        let full = fmt_hms(r.start_time);
        let t = full.get(5..).unwrap_or("");
        let pace = if r.total_dis > 0.0 && r.total_time > 0 {
            let p = r.total_time as f64 / (r.total_dis / 1000.0);
            format!("{}:{:02}", (p / 60.0) as i64, (p as i64) % 60)
        } else {
            "-".into()
        };
        println!(
            "{:<16} {:>8.0} {:>8} {:>6} {:>6} {:>4} {}",
            t,
            r.total_dis,
            format!(
                "{}:{:02}:{:02}",
                r.total_time / 3600,
                r.total_time % 3600 / 60,
                r.total_time % 60
            ),
            pace,
            r.avg_step_freq,
            if r.complete { "是" } else { "否" },
            r.rrid
        );
    }
}

pub(crate) fn jstr(v: &serde_json::Value, keys: &[&str]) -> String {
    for k in keys {
        match v.get(k) {
            Some(serde_json::Value::String(s)) => return s.clone(),
            Some(serde_json::Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    "-".into()
}

/// 配速：`m:ss` 或秒数（每公里），必须为正。
pub(crate) fn parse_pace(v: &str) -> Result<f32, String> {
    let v = v.trim();
    let secs = if let Some((m, s)) = v.split_once(':') {
        match (m.parse::<u32>(), s.parse::<u32>()) {
            (Ok(m), Ok(s)) if s < 60 && m < 600 => Some((m * 60 + s) as f32),
            _ => None,
        }
    } else {
        v.parse::<f32>().ok().filter(|x| x.is_finite())
    };
    secs.filter(|x| *x > 0.0)
        .ok_or_else(|| format!("--pace 需为 m:ss 或秒数（大于 0），收到 {v:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_VALUES: &[&str] = &[
        "dist", "pace", "altitude", "ago", "days-ago", "time", "face", "seed", "route",
    ];

    #[test]
    fn value_flags_take_dash_values() {
        let f = parse_flags(
            &["--user", "138", "--pass", "-Abc123", "--remember"],
            &["user", "pass"],
            &["remember"],
        )
        .unwrap();
        assert_eq!(get(&f, "pass"), Some("-Abc123"));
        assert_eq!(get(&f, "user"), Some("138"));
        assert_eq!(get(&f, "remember"), Some("1"));
        // 负数等以 - 开头的值同样作为值
        let f = parse_flags(&["--ago", "-5"], RUN_VALUES, &[]).unwrap();
        assert_eq!(get(&f, "ago"), Some("-5"));
    }

    #[test]
    fn key_equals_value() {
        let f = parse_flags(
            &["--dist=2.5", "--time=07:30", "--pass=a=b"],
            &["dist", "time", "pass"],
            &[],
        )
        .unwrap();
        assert_eq!(get(&f, "dist"), Some("2.5"));
        assert_eq!(get(&f, "time"), Some("07:30"));
        assert_eq!(get(&f, "pass"), Some("a=b"));
        let f = parse_flags(&["--body="], &["body"], &[]).unwrap();
        assert_eq!(get(&f, "body"), Some(""));
    }

    #[test]
    fn rejects_unknown_missing_and_positional() {
        assert!(parse_flags(&["--bogus", "1"], RUN_VALUES, &[])
            .unwrap_err()
            .contains("未知参数"));
        assert!(parse_flags(&["--dist"], RUN_VALUES, &[])
            .unwrap_err()
            .contains("缺少参数值"));
        assert!(parse_flags(&["stray"], RUN_VALUES, &[]).is_err());
        assert!(parse_flags(&["--"], RUN_VALUES, &[]).is_err());
        assert!(parse_flags(&["---dist", "1"], RUN_VALUES, &[]).is_err());
        assert!(parse_flags(&["--check=1"], &[], &["check"])
            .unwrap_err()
            .contains("开关"));
        assert!(parse_flags(&["--x"], &[], &[])
            .unwrap_err()
            .contains("不接受参数"));
        assert!(parse_flags(&[], &[], &[]).unwrap().is_empty());
    }

    #[test]
    fn later_flag_wins_and_single_dash_accepted() {
        let f = parse_flags(&["-dist", "1", "--dist", "2"], RUN_VALUES, &[]).unwrap();
        assert_eq!(get(&f, "dist"), Some("2"));
    }

    #[test]
    fn numbers_are_validated() {
        let f = parse_flags(
            &[
                "--dist",
                "abc",
                "--seed",
                "x",
                "--page",
                "3",
                "--days-ago",
                "9",
            ],
            &["dist", "seed", "page", "days-ago"],
            &[],
        )
        .unwrap();
        assert!(get_pos_f64(&f, "dist").is_err());
        assert!(get_int::<u64>(&f, "seed").is_err());
        assert_eq!(get_int::<i64>(&f, "page").unwrap(), Some(3));
        assert!(get_int_in(&f, "days-ago", 0, 3).is_err());
        assert_eq!(get_int::<i64>(&f, "missing").unwrap(), None);
        for bad in ["0", "-1", "nan", "inf"] {
            let f = parse_flags(&["--dist", bad], &["dist"], &[]).unwrap();
            assert!(get_pos_f64(&f, "dist").is_err(), "{bad}");
        }
    }

    #[test]
    fn pace_parsing() {
        assert_eq!(parse_pace("6:30"), Ok(390.0));
        assert_eq!(parse_pace("400"), Ok(400.0));
        for bad in ["abc", "6:60", "0", "-5", "6:xx", ":", "nan"] {
            assert!(parse_pace(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn password_sources() {
        assert_eq!(
            password_from(Some("-p"), Some("env".into())).as_deref(),
            Some("-p")
        );
        assert_eq!(
            password_from(None, Some("env".into())).as_deref(),
            Some("env")
        );
        assert_eq!(password_from(None, Some(String::new())), None);
        assert_eq!(password_from(None, None), None);
    }

    #[test]
    fn fmt_hms_slice_is_safe() {
        assert_eq!(fmt_hms(i64::MAX).get(5..).unwrap_or(""), "");
    }
}
