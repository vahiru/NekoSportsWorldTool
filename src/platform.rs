//! Platform services shared by the existing UI and the Android host.

#[derive(Clone, Copy, Debug)]
pub enum InputKind {
    Text,
    Password,
    Integer,
    Decimal,
}

pub fn data_dir() -> std::path::PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_DATA_DIR.with(|p| p.borrow().clone()) {
        return path;
    }
    #[cfg(feature = "flutter")]
    if let Some(path) = FLUTTER_DATA_DIR.with(|p| p.borrow().clone()) {
        return path;
    }
    #[cfg(all(target_os = "android", feature = "android"))]
    {
        crate::android::data_dir()
    }
    #[cfg(not(all(target_os = "android", feature = "android")))]
    {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        bundle_data_dir(&exe_dir, std::env::var_os("HOME")).unwrap_or(exe_dir)
    }
}

/// macOS .app 内运行时（…/X.app/Contents/MacOS）数据不能写进包里（破坏签名、
/// 更新或移动 .app 即丢失），改用 ~/Library/Application Support/NekoSportsWorldTool。
/// 裸二进制仍沿用「程序同目录」约定。
#[cfg(not(all(target_os = "android", feature = "android")))]
fn bundle_data_dir(
    exe_dir: &std::path::Path,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "macos") || !exe_dir.ends_with("Contents/MacOS") {
        return None;
    }
    let bundle = exe_dir.parent()?.parent()?;
    if bundle.extension()? != "app" {
        return None;
    }
    let dir =
        std::path::PathBuf::from(home?).join("Library/Application Support/NekoSportsWorldTool");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn edit_text(id: u64, value: &str, kind: InputKind) {
    #[cfg(all(target_os = "android", feature = "android"))]
    crate::android::edit_text(id, value, kind);
    #[cfg(not(all(target_os = "android", feature = "android")))]
    let _ = (id, value, kind);
}

pub fn take_edited_text(id: u64) -> Option<String> {
    #[cfg(all(target_os = "android", feature = "android"))]
    {
        crate::android::take_edited_text(id)
    }
    #[cfg(not(all(target_os = "android", feature = "android")))]
    {
        let _ = id;
        None
    }
}

pub fn set_keep_screen_on(enabled: bool) {
    #[cfg(all(target_os = "android", feature = "android"))]
    crate::android::set_keep_screen_on(enabled);
    #[cfg(not(all(target_os = "android", feature = "android")))]
    let _ = enabled;
}

/// 当前程序版本：桌面读 Cargo 版本，Android 读 manifest versionName
/// （形如 0.2.5-android.3，与 Release tag 对齐）。
/// Android 端经 JNI 读取，结果缓存（界面每帧都会显示版本号）。
pub fn version_name() -> String {
    #[cfg(all(target_os = "android", feature = "android"))]
    {
        static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        if let Some(cached) = CACHE.get() {
            return cached.clone();
        }
        let value = crate::android::version_name();
        // Activity 未就绪时读到空串，不缓存，下次再试
        if !value.is_empty() {
            let _ = CACHE.set(value.clone());
        }
        value
    }
    #[cfg(not(all(target_os = "android", feature = "android")))]
    {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

/// Android：把已下载的 APK 交给系统安装器（用户在系统弹窗确认安装）。
pub fn install_apk(path: &str) {
    #[cfg(all(target_os = "android", feature = "android"))]
    crate::android::install_apk(path);
    #[cfg(not(all(target_os = "android", feature = "android")))]
    let _ = path;
}

#[cfg(feature = "gui")]
pub fn sync_clipboard(context: &egui::Context) {
    #[cfg(all(target_os = "android", feature = "android"))]
    {
        // Only the last copy of the frame matters: it is what ends up on the clipboard.
        let text = context.output(|output| {
            output
                .commands
                .iter()
                .rev()
                .find_map(|command| match command {
                    egui::OutputCommand::CopyText(text) => Some(text.clone()),
                    _ => None,
                })
        });
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            crate::android::copy_text(&text);
        }
    }
    #[cfg(not(all(target_os = "android", feature = "android")))]
    let _ = context;
}

#[cfg(feature = "gui")]
pub fn apply_safe_area(context: &egui::Context, input: &mut egui::RawInput) {
    #[cfg(all(target_os = "android", feature = "android"))]
    if let Some(rect) = input.screen_rect.as_mut() {
        let [left, top, right, bottom] = crate::android::safe_insets();
        let scale = context.pixels_per_point();
        rect.min.x += left as f32 / scale;
        rect.min.y += top as f32 / scale;
        rect.max.x -= right as f32 / scale;
        rect.max.y -= bottom as f32 / scale;
    }
    #[cfg(not(all(target_os = "android", feature = "android")))]
    let _ = (context, input);
}

// Flutter 宿主按账号给每个工作线程指定应用私有目录；不改 cwd，也不是全局状态。
#[cfg(feature = "flutter")]
thread_local! {
    static FLUTTER_DATA_DIR: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "flutter")]
pub(crate) fn set_data_dir(path: std::path::PathBuf) {
    FLUTTER_DATA_DIR.with(|p| *p.borrow_mut() = Some(path));
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_DATA_DIR: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, target_os = "macos"))]
mod bundle_dir_tests {
    use super::bundle_data_dir;
    use std::path::Path;

    #[test]
    fn app_bundle_uses_application_support_and_bare_binary_does_not() {
        let home = std::env::temp_dir().join(format!("nswt-home-{}", std::process::id()));
        let got = bundle_data_dir(
            Path::new("/Applications/NekoSportsWorldTool.app/Contents/MacOS"),
            Some(home.clone().into()),
        )
        .unwrap();
        assert_eq!(
            got,
            home.join("Library/Application Support/NekoSportsWorldTool")
        );
        assert!(got.is_dir());
        assert!(
            bundle_data_dir(Path::new("/Users/x/Downloads"), Some(home.clone().into())).is_none()
        );
        assert!(
            bundle_data_dir(Path::new("/opt/Contents/MacOS"), Some(home.clone().into())).is_none()
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
