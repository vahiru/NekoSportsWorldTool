//! 中文字体安装。
//!
//! 启动时按平台从系统字体目录加载中文字体注入 egui：
//! Windows 微软雅黑 / 黑体 / 宋体，macOS 冬青黑体 / 华文黑体 / 宋体，
//! Linux Noto CJK / 文泉驿；全部失败返回 None（不崩溃，
//! 界面中文会显示为方块但不影响功能）。

#[cfg(not(target_os = "android"))]
use std::path::PathBuf;

#[cfg(target_os = "android")]
const ANDROID_FONT: &[u8] = include_bytes!("../../assets/fonts/NotoSansSC-VF.ttf");

/// 字体候选（按优先级，完整路径）。
#[cfg(target_os = "windows")]
fn candidates() -> Vec<PathBuf> {
    let dir = std::env::var("WINDIR")
        .map(|w| PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows\Fonts"));
    ["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"]
        .iter()
        .map(|name| dir.join(name))
        .collect()
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<PathBuf> {
    [
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "android")))]
fn candidates() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

/// 安装中文字体到 egui。返回实际加载的字体名（None = 全部失败）。
pub fn install(ctx: &egui::Context) -> Option<String> {
    #[cfg(target_os = "android")]
    {
        let name = "Noto Sans SC".to_owned();
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert(name.clone(), egui::FontData::from_static(ANDROID_FONT));
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, name.clone());
        }
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            list.push(name.clone());
        }
        ctx.set_fonts(fonts);
        return Some(name);
    }

    #[cfg(not(target_os = "android"))]
    {
    for path in candidates() {
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        // ttc 集合取 index 0（msyh.ttc[0] = Microsoft YaHei）
        fonts
            .font_data
            .insert(name.to_string(), egui::FontData::from_owned(data));
        // proportional：插到最前（中文优先由系统中文字体渲染）
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, name.to_string());
        }
        // monospace：追加（数字/ASCII 仍用默认等宽，中文回落中文字体）
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            list.push(name.to_string());
        }
        ctx.set_fonts(fonts);
        return Some(name.to_string());
    }
    None
    }
}
