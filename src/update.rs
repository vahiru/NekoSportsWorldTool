//! 自动更新：GitHub Release 检查 / 下载 / 解包 / 自替换。
//!
//! 桌面产物（win zip / mac arm64+x64 tar.gz / linux x64+aarch64 纯 CLI tar.gz）只查上游仓库；
//! Android 查上游 + fork 两个仓库（上游发 APK 前先落在 fork，发了则
//! 自动取更新的那个），取带 APK 资产且版本最新的 Release。
//! NSWT_UPDATE_REPO 环境变量可强制指定仓库（测试用）。
//! 版本号 / 重启 / 安装的平台差异见 platform.rs。

use crate::api::client::{make_agent, ureq_err};

pub const REPO_UPSTREAM: &str = "YanamiNeko/NekoSportsWorldTool";
#[cfg_attr(not(any(feature = "gui", target_os = "android")), allow(dead_code))]
pub const REPO_FORK: &str = "hb-degithub/NekoSportsWorldTool";

const UA: &str = concat!("NekoSportsWorldTool/", env!("CARGO_PKG_VERSION"));

/// Android 更新包落盘文件名（私有目录，ApkProvider 只暴露这一个文件）。
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub const APK_NAME: &str = "update.apk";

/// 检查更新要查询的仓库（按序）。
fn repos() -> Vec<String> {
    if let Ok(r) = std::env::var("NSWT_UPDATE_REPO") {
        if !r.trim().is_empty() {
            return vec![r.trim().to_string()];
        }
    }
    #[cfg(target_os = "android")]
    let list = [REPO_UPSTREAM, REPO_FORK];
    #[cfg(not(target_os = "android"))]
    let list = [REPO_UPSTREAM];
    list.iter().map(|s| s.to_string()).collect()
}

/// 当前平台对应的 Release 资产名；无匹配产物的平台返回 None。
pub fn asset_name() -> Option<&'static str> {
    asset_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn asset_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("NekoSportsWorldTool-win-x64.zip"),
        ("macos", "aarch64") => Some("NekoSportsWorldTool-macos-arm64.tar.gz"),
        ("macos", "x86_64") => Some("NekoSportsWorldTool-macos-x64.tar.gz"),
        ("linux", "x86_64") => Some("NekoSportsWorldTool-linux-x64.tar.gz"),
        ("linux", "aarch64") => Some("NekoSportsWorldTool-linux-aarch64.tar.gz"),
        ("android", "aarch64") => Some("NekoSportsWorldTool-android-arm64.apk"),
        _ => None,
    }
}

/// 发布包与当前构建是否兼容：Linux 发布包是 `--no-default-features` 的纯 CLI
/// 构建，自行编译的 Linux GUI 版若自更新会被替换成无界面版本，因此拒绝。
fn check_build_compat(os: &str, gui: bool) -> Result<(), String> {
    if os == "linux" && gui {
        return Err("Linux 发布包为纯 CLI 构建，当前为 GUI 构建，自动更新会丢失图形界面；\
                    请拉取新源码重新编译（cargo build --release）"
            .into());
    }
    Ok(())
}

/// 当前程序版本（platform.rs：桌面读 Cargo，Android 读 manifest versionName）。
pub fn current_version() -> String {
    crate::platform::version_name()
}

/// Release 摘要（GUI / CLI 共用，经 channel JSON 传递）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReleaseInfo {
    pub repo: String,
    pub tag: String,
    pub title: String,
    pub notes: String,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
    /// 同一 Release 内 `<资产名>.sha256` 的下载地址；旧版 Release 没有则为空。
    #[serde(default)]
    pub sha256_url: String,
}

/// 检查最新 Release；比当前新返回 Some，已是最新返回 None。
/// 多仓库（Android）时取带资产且版本最新的候选。
pub fn check_latest(log: &mut dyn FnMut(&str)) -> Result<Option<ReleaseInfo>, String> {
    let want = asset_name().ok_or_else(|| "当前平台没有对应的发布产物，无法自动更新".to_string())?;
    check_build_compat(std::env::consts::OS, cfg!(feature = "gui"))?;
    let current = current_version();
    let mut best: Option<ReleaseInfo> = None;
    let mut last_err: Option<String> = None;
    for repo in repos() {
        match fetch_release(&repo, want, log) {
            Ok(Some(rel)) => {
                if best.as_ref().map(|b| is_newer(&b.tag, &rel.tag)).unwrap_or(true) {
                    best = Some(rel);
                }
            }
            Ok(None) => {}
            Err(e) => {
                log(&format!("[update] {repo}: {e}"));
                last_err = Some(e);
            }
        }
    }
    match best {
        Some(rel) => {
            if is_newer(&current, &rel.tag) {
                log(&format!(
                    "√ [update] {} 最新 {}（当前 v{}，{:.1} MB）",
                    rel.repo,
                    rel.tag,
                    current,
                    rel.asset_size as f64 / 1024.0 / 1024.0
                ));
                Ok(Some(rel))
            } else {
                log(&format!("√ [update] 已是最新版本（v{current}）"));
                Ok(None)
            }
        }
        None => Err(last_err.unwrap_or_else(|| "更新源没有可用的发布资产".into())),
    }
}

/// 拉取单仓库最新 Release 并匹配资产；Release 存在但无匹配资产 → Ok(None)。
fn fetch_release(repo: &str, want: &str, log: &mut dyn FnMut(&str)) -> Result<Option<ReleaseInfo>, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let resp = make_agent()
        .get(&url)
        .set("User-Agent", UA)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(ureq_err)?;
    let text = resp.into_string().map_err(|e| format!("读取 Release 失败: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 Release 失败: {e}"))?;
    let tag = v.get("tag_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if tag.is_empty() {
        return Err("Release 缺少 tag".into());
    }
    let Some(asset) = pick_asset(&v, want) else {
        return Ok(None);
    };
    log(&format!("[update] {repo} 最新 {tag}，匹配资产 {}", asset.name));
    let sha_name = format!("{}.sha256", asset.name);
    let sha256_url = pick_asset(&v, &sha_name).map(|a| a.url).unwrap_or_default();
    Ok(Some(ReleaseInfo {
        repo: repo.to_string(),
        tag,
        title: v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        notes: v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        asset_name: asset.name,
        asset_url: asset.url,
        asset_size: asset.size,
        sha256_url,
    }))
}

struct AssetPick {
    name: String,
    url: String,
    size: u64,
}

/// 资产匹配：桌面产物要求名字完全一致；APK 先精确后模糊（兼容历史命名）。
fn pick_asset(v: &serde_json::Value, want: &str) -> Option<AssetPick> {
    let assets = v.get("assets")?.as_array()?;
    let find = |pred: &dyn Fn(&str) -> bool| -> Option<AssetPick> {
        assets.iter().find_map(|a| {
            let name = a.get("name")?.as_str()?;
            if !pred(name) {
                return None;
            }
            Some(AssetPick {
                name: name.to_string(),
                url: a.get("browser_download_url")?.as_str()?.to_string(),
                size: a.get("size")?.as_u64().unwrap_or(0),
            })
        })
    };
    if want.ends_with(".apk") {
        find(&|n| n == want)
            .or_else(|| find(&|n| n.ends_with(".apk") && n.contains("arm64")))
            .or_else(|| find(&|n| n.ends_with(".apk")))
    } else {
        find(&|n| n == want)
    }
}

/// 版本比较：剥 v 前缀按点分段数字比较；主版本相同时看后缀。
pub fn is_newer(current: &str, latest: &str) -> bool {
    is_newer_for(current, latest, cfg!(target_os = "android"))
}

/// 后缀规则：-android.N 是 APK 修订号，只在 Android 上比 N，桌面视同正式版
/// （否则安卓 tag 会让桌面反复提示同一版本）；其他后缀（-rc.1 / -beta）为预发布，低于正式版。
fn is_newer_for(current: &str, latest: &str, android: bool) -> bool {
    let (c_base, c_rank) = split_rev(current, android);
    let (l_base, l_rank) = split_rev(latest, android);
    if c_base != l_base {
        return l_base > c_base;
    }
    l_rank > c_rank
}

/// "v0.2.5-android.3" → ([0,2,5], 后缀排序值)：预发布 0 < 正式版 1 < android.N 为 2+N。
fn split_rev(v: &str, android: bool) -> (Vec<u64>, u64) {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (base, rank) = match v.split_once('-') {
        Some((b, r)) => match r.strip_prefix("android.").and_then(|n| n.parse::<u64>().ok()) {
            Some(n) if android => (b, n.saturating_add(2)),
            Some(_) => (b, 1),
            None => (b, 0),
        },
        None => (v, 1),
    };
    let segs = base.split('.').map(|s| s.parse::<u64>().unwrap_or(0)).collect();
    (segs, rank)
}

/// 流式下载（默认 agent 30s 总超时不够下几 MB 的包，专用长超时）；
/// 超过 limit 字节立即中止。
fn download_limited(url: &str, limit: u64, mut progress: impl FnMut(u64, Option<u64>)) -> Result<Vec<u8>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(300))
        .build();
    let resp = agent.get(url).set("User-Agent", UA).call().map_err(ureq_err)?;
    let total = resp.header("Content-Length").and_then(|h| h.parse::<u64>().ok());
    let mut buf = Vec::new();
    let mut reader = resp.into_reader();
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = std::io::Read::read(&mut reader, &mut chunk)
            .map_err(|e| format!("下载中断: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() as u64 > limit {
            return Err(format!("下载内容超过预期大小（{limit} 字节），已中止"));
        }
        progress(buf.len() as u64, total);
    }
    Ok(buf)
}

/// 下载 Release 资产并校验：大小必须与 Release 声明一致；Release 附带
/// `<资产名>.sha256` 时再比对 SHA-256。校验失败不返回数据，调用方不会替换程序。
pub fn download_verified(rel: &ReleaseInfo, progress: impl FnMut(u64, Option<u64>)) -> Result<Vec<u8>, String> {
    if rel.asset_size == 0 {
        return Err("Release 未声明资产大小，拒绝更新".into());
    }
    let bytes = download_limited(&rel.asset_url, rel.asset_size, progress)?;
    if bytes.len() as u64 != rel.asset_size {
        return Err(format!("下载大小不符（{} / {} 字节），已放弃更新", bytes.len(), rel.asset_size));
    }
    if rel.sha256_url.is_empty() {
        return Ok(bytes);
    }
    let sums = download_limited(&rel.sha256_url, 4096, |_, _| {})?;
    let sums = String::from_utf8_lossy(&sums);
    let want = sums
        .split_whitespace()
        .next()
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("SHA-256 校验文件格式异常，已放弃更新")?
        .to_ascii_lowercase();
    use sha2::Digest;
    let got = format!("{:x}", sha2::Sha256::digest(&bytes));
    if got != want {
        return Err("SHA-256 校验失败，下载内容与发布不一致，已放弃更新".into());
    }
    Ok(bytes)
}

/// 从发布包解出新程序二进制。桌面：zip 内找 *.exe（7z 打包带 target/
/// 目录前缀，取最大的一条）；tar.gz 内找根级 nekosportsworldtool。
#[cfg(not(target_os = "android"))]
pub fn extract(asset_name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if asset_name.ends_with(".zip") {
        extract_zip(bytes)
    } else if asset_name.ends_with(".tar.gz") || asset_name.ends_with(".tgz") {
        extract_tar_gz(bytes)
    } else if asset_name.ends_with(".apk") {
        Ok(bytes.to_vec())
    } else {
        Err(format!("不支持的发布包格式：{asset_name}"))
    }
}

/// Android 无需解包（APK 整包交给系统安装器）。
#[cfg(target_os = "android")]
pub fn extract(asset_name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if asset_name.ends_with(".apk") {
        Ok(bytes.to_vec())
    } else {
        Err(format!("不支持的发布包格式：{asset_name}"))
    }
}

#[cfg(not(target_os = "android"))]
fn extract_zip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("zip 打开失败: {e}"))?;
    let mut best: Option<Vec<u8>> = None;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| format!("zip 读取失败: {e}"))?;
        if f.is_dir() {
            continue;
        }
        let is_exe = std::path::Path::new(f.name())
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if !is_exe {
            continue;
        }
        let mut out = Vec::with_capacity(f.size() as usize);
        std::io::Read::read_to_end(&mut f, &mut out).map_err(|e| format!("zip 解压失败: {e}"))?;
        if best.as_ref().map(|b| out.len() > b.len()).unwrap_or(true) {
            best = Some(out);
        }
    }
    best.ok_or_else(|| "zip 内未找到程序文件".into())
}

#[cfg(not(target_os = "android"))]
fn extract_tar_gz(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut tar = tar::Archive::new(gz);
    let mut best: Option<Vec<u8>> = None;
    let entries = tar.entries().map_err(|e| format!("tar 打开失败: {e}"))?;
    for entry in entries {
        let mut e = entry.map_err(|e| format!("tar 读取失败: {e}"))?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let path = e.path().map_err(|e| format!("tar 路径异常: {e}"))?;
        if path.file_name().and_then(|n| n.to_str()) != Some("nekosportsworldtool") {
            continue;
        }
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut e, &mut out).map_err(|e| format!("tar 解压失败: {e}"))?;
        if best.as_ref().map(|b| out.len() > b.len()).unwrap_or(true) {
            best = Some(out);
        }
    }
    best.ok_or_else(|| "tar.gz 内未找到程序文件".into())
}

/// 自替换当前程序。调用方随后退出。
#[cfg(not(target_os = "android"))]
pub fn apply(binary: &[u8]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位自身路径: {e}"))?;
    apply_to(&exe, binary)
}

/// 新二进制先落同目录 `<exe>.new` 并落盘；
/// - Unix：沿用原文件权限后单次 `rename(.new, exe)` 原子替换（运行中的进程仍持有旧 inode），
///   任一时刻 exe 路径要么是旧版要么是新版；
/// - Windows：运行中的 exe 不能被覆盖但可改名，故 exe → .old、.new → exe 两步，失败回滚；
///   残留 .old 由下次启动 `cleanup_residue` 清理。
#[cfg(not(target_os = "android"))]
fn apply_to(exe: &std::path::Path, binary: &[u8]) -> Result<(), String> {
    let dir = exe.parent().ok_or("无法定位 exe 目录")?;
    let file = exe.file_name().and_then(|n| n.to_str()).ok_or("exe 文件名异常")?;
    let new = dir.join(format!("{file}.new"));
    let write_new = || -> Result<(), String> {
        use std::io::Write;
        let mut f = std::fs::File::create(&new)
            .map_err(|e| format!("写入更新文件失败（目录可能只读）: {e}"))?;
        f.write_all(binary).map_err(|e| format!("写入更新内容失败: {e}"))?;
        f.sync_all().map_err(|e| format!("落盘失败: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 沿用原程序权限（保留用户 / 组 / 其他位），读不到时退回 0755
            let mode = std::fs::metadata(exe)
                .map(|m| m.permissions().mode() & 0o7777)
                .unwrap_or(0o755);
            std::fs::set_permissions(&new, std::fs::Permissions::from_mode(mode))
                .map_err(|e| format!("设置执行权限失败: {e}"))?;
        }
        Ok(())
    };
    if let Err(e) = write_new() {
        let _ = std::fs::remove_file(&new);
        return Err(e);
    }
    #[cfg(unix)]
    {
        std::fs::rename(&new, exe).map_err(|e| {
            let _ = std::fs::remove_file(&new);
            format!("替换程序失败: {e}")
        })?;
        // 目录项落盘（尽力而为），避免掉电后回到旧版本或丢失文件
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let old = dir.join(format!("{file}.old"));
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old).map_err(|e| {
            let _ = std::fs::remove_file(&new);
            format!("无法移动当前程序（可能被占用）: {e}")
        })?;
        if let Err(e) = std::fs::rename(&new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(format!("替换程序失败: {e}"));
        }
        Ok(())
    }
}

/// 清理上次更新残留的 .old/.new（Windows 两步替换留下的 .old、任一平台中断留下的
/// .new；旧进程未退出时会删不掉，静默跳过）。
pub fn cleanup_residue() {
    let Ok(exe) = std::env::current_exe() else { return };
    cleanup_residue_for(&exe);
}

fn cleanup_residue_for(exe: &std::path::Path) {
    let Some(dir) = exe.parent() else { return };
    let Some(file) = exe.file_name().and_then(|n| n.to_str()) else { return };
    for suffix in [".old", ".new"] {
        let _ = std::fs::remove_file(dir.join(format!("{file}{suffix}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.2.5", "v0.2.6"));
        assert!(is_newer("0.2.9", "v0.3.0"));
        assert!(is_newer("0.2.9", "v0.10.0"));
        assert!(!is_newer("0.2.5", "v0.2.5"));
        assert!(!is_newer("0.2.6", "v0.2.5"));
        // android 后缀序号
        assert!(is_newer_for("0.2.5-android.3", "v0.2.5-android.4", true));
        assert!(!is_newer_for("0.2.5-android.4", "v0.2.5-android.4", true));
        assert!(is_newer_for("0.2.5", "v0.2.5-android.1", true));
        assert!(is_newer_for("0.2.5", "v0.2.6-android.1", true));
        assert!(is_newer_for("0.2.5-android.99", "v0.2.6-android.1", true));
        // 桌面：安卓修订号 tag 不算新版本，预发布低于正式版
        assert!(!is_newer_for("0.3.0", "v0.3.0-android.1", false));
        assert!(is_newer_for("0.3.0", "v0.3.1-android.1", false));
        assert!(!is_newer_for("0.3.0", "v0.3.0-rc.1", false));
        assert!(!is_newer_for("0.3.0", "v0.3.0-rc.1", true));
        assert!(is_newer_for("0.3.0-rc.1", "v0.3.0", false));
        // 位数不同 / 前后空格
        assert!(is_newer("0.2", "v0.2.1"));
        assert!(!is_newer(" 0.2.5 ", "v0.2.5"));
    }

    #[test]
    fn asset_pick_exact_and_fuzzy() {
        let mk = |names: &[&str]| {
            serde_json::json!({
                "assets": names.iter().map(|n| serde_json::json!({
                    "name": n,
                    "size": 100,
                    "browser_download_url": format!("https://x/{n}"),
                })).collect::<Vec<_>>(),
            })
        };
        let exact = pick_asset(&mk(&["NekoSportsWorldTool-win-x64.zip", "other.txt"]), "NekoSportsWorldTool-win-x64.zip");
        assert_eq!(exact.unwrap().name, "NekoSportsWorldTool-win-x64.zip");
        // 桌面产物必须精确匹配
        assert!(pick_asset(&mk(&["wrong.zip"]), "NekoSportsWorldTool-win-x64.zip").is_none());
        // APK 模糊匹配：先精确、再 arm64、再任意 apk
        let apk = pick_asset(&mk(&["NekoSportsWorldTool-0.2.5-android.2-arm64.apk"]), "NekoSportsWorldTool-android-arm64.apk");
        assert_eq!(apk.unwrap().name, "NekoSportsWorldTool-0.2.5-android.2-arm64.apk");
        let any = pick_asset(&mk(&["app-arm64.apk", "plain.apk"]), "NekoSportsWorldTool-android-arm64.apk");
        assert_eq!(any.unwrap().name, "app-arm64.apk");
    }

    #[test]
    fn asset_mapping_and_linux_gui_guard() {
        assert_eq!(asset_for("macos", "x86_64"), Some("NekoSportsWorldTool-macos-x64.tar.gz"));
        assert_eq!(asset_for("macos", "aarch64"), Some("NekoSportsWorldTool-macos-arm64.tar.gz"));
        assert_eq!(asset_for("linux", "x86_64"), Some("NekoSportsWorldTool-linux-x64.tar.gz"));
        assert_eq!(asset_for("windows", "aarch64"), None);
        assert!(check_build_compat("linux", true).unwrap_err().contains("CLI"));
        assert!(check_build_compat("linux", false).is_ok());
        assert!(check_build_compat("macos", true).is_ok());
        assert!(check_build_compat("windows", true).is_ok());
        assert!(check_build_compat("android", true).is_ok());
    }

    #[cfg(not(target_os = "android"))]
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nswt-update-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn apply_replaces_and_keeps_permissions() {
        let dir = scratch_dir("apply");
        let exe = dir.join("nekosportsworldtool");
        std::fs::write(&exe, b"OLD").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o750)).unwrap();
        }
        apply_to(&exe, b"NEW_BINARY").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"NEW_BINARY");
        assert!(!dir.join("nekosportsworldtool.new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode, 0o750);
            // Unix 单步原子替换，不留 .old
            assert!(!dir.join("nekosportsworldtool.old").exists());
        }
        // 残留清理
        std::fs::write(dir.join("nekosportsworldtool.old"), b"x").unwrap();
        std::fs::write(dir.join("nekosportsworldtool.new"), b"x").unwrap();
        cleanup_residue_for(&exe);
        assert!(!dir.join("nekosportsworldtool.old").exists());
        assert!(!dir.join("nekosportsworldtool.new").exists());
        assert!(exe.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn extract_zip_nested_exe() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            w.start_file("target/x86_64-pc-windows-msvc/release/nekosportsworldtool.exe", opt).unwrap();
            std::io::Write::write_all(&mut w, b"BIN_CONTENT").unwrap();
            w.start_file("readme.txt", opt).unwrap();
            std::io::Write::write_all(&mut w, b"ignore").unwrap();
            w.finish().unwrap();
        }
        let bytes = buf.into_inner();
        assert_eq!(extract_zip(&bytes).unwrap(), b"BIN_CONTENT");
        assert_eq!(extract("NekoSportsWorldTool-win-x64.zip", &bytes).unwrap(), b"BIN_CONTENT");
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn extract_tar_gz_root_binary() {
        let mut bytes = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
            let mut tar = tar::Builder::new(gz);
            let mut hdr = tar::Header::new_gnu();
            let content: &[u8] = b"BIN_CONTENT";
            hdr.set_size(content.len() as u64);
            hdr.set_mode(0o755);
            hdr.set_cksum();
            tar.append_data(&mut hdr, "nekosportsworldtool", content).unwrap();
            let gz = tar.into_inner().unwrap();
            gz.finish().unwrap();
        }
        assert_eq!(extract_tar_gz(&bytes).unwrap(), b"BIN_CONTENT");
        assert_eq!(extract("NekoSportsWorldTool-linux-x64.tar.gz", &bytes).unwrap(), b"BIN_CONTENT");
    }

    /// 本机回环 HTTP：按请求顺序返回给定内容。
    fn serve(bodies: Vec<Vec<u8>>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for body in bodies {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                s.write_all(&body).unwrap();
            }
        });
        host
    }

    fn release(host: &str, size: u64, with_sha: bool) -> ReleaseInfo {
        ReleaseInfo {
            repo: "fixture".into(),
            tag: "v9.9.9".into(),
            title: String::new(),
            notes: String::new(),
            asset_name: "a.tar.gz".into(),
            asset_url: format!("{host}/a.tar.gz"),
            asset_size: size,
            sha256_url: if with_sha { format!("{host}/a.tar.gz.sha256") } else { String::new() },
        }
    }

    #[test]
    fn verified_download_checks_size_and_sha256() {
        use sha2::Digest;
        let body = b"NEW_BINARY".to_vec();
        let good = format!("{:x}  a.tar.gz\n", sha2::Sha256::digest(&body)).into_bytes();
        let host = serve(vec![body.clone(), good]);
        assert_eq!(download_verified(&release(&host, 10, true), |_, _| {}).unwrap(), body);

        let host = serve(vec![body.clone(), format!("{}  a.tar.gz\n", "0".repeat(64)).into_bytes()]);
        assert!(download_verified(&release(&host, 10, true), |_, _| {}).unwrap_err().contains("SHA-256"));

        let host = serve(vec![body.clone()]);
        assert!(download_verified(&release(&host, 11, false), |_, _| {}).unwrap_err().contains("大小不符"));

        let host = serve(vec![body]);
        assert!(download_verified(&release(&host, 4, false), |_, _| {}).unwrap_err().contains("超过预期"));

        assert!(download_verified(&release("http://127.0.0.1:9", 0, false), |_, _| {}).is_err());
    }
}
