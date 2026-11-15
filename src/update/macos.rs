//! Adapted from LightWhite520/d174361: updating an executable also invalidates
//! its .app seal. Sign a staged bundle, then atomically swap the whole bundle.
use std::{ffi::CString, os::unix::ffi::OsStrExt, path::Path, process::Command};

pub(super) fn bundle_root(exe: &Path) -> Option<&Path> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
}

pub(super) fn apply(bundle: &Path, exe: &Path, binary: &[u8]) -> Result<(), String> {
    // This updater supports our ad-hoc signed releases. Preserve a developer's
    // identity rather than silently replacing their signature with ad-hoc code.
    let signature = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=2"])
        .arg(bundle)
        .output()
        .map_err(|e| format!("无法检查应用签名：{e}"))?;
    if signature.status.success()
        && !String::from_utf8_lossy(&signature.stderr).contains("Signature=adhoc")
    {
        return Err("当前应用使用开发者签名，请下载完整应用包更新以保留签名".into());
    }
    stage_and_apply(bundle, exe, binary, sign_and_verify)
}

struct Stage(std::path::PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stage_and_apply(
    bundle: &Path,
    exe: &Path,
    binary: &[u8],
    sign: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let parent = bundle.parent().ok_or("无法定位应用所在目录")?;
    let relative_exe = exe.strip_prefix(bundle).map_err(|_| "程序不在应用包内")?;
    let meta = std::fs::symlink_metadata(bundle).map_err(|e| format!("无法读取应用包：{e}"))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("应用包不是普通目录，请下载完整应用包更新".into());
    }
    // A sibling staging directory guarantees the same filesystem for RENAME_SWAP.
    let stage = Stage(parent.join(format!(".neko-update-{}", uuid::Uuid::new_v4().simple())));
    std::fs::create_dir(&stage.0).map_err(|e| format!("无法建立更新目录：{e}"))?;
    let next = stage.0.join(bundle.file_name().ok_or("应用包名称无效")?);
    let copied = Command::new("/usr/bin/ditto")
        .arg(bundle)
        .arg(&next)
        .output()
        .map_err(|e| format!("无法暂存应用包：{e}"))?;
    if !copied.status.success() {
        return Err("暂存应用包失败，原程序未修改".into());
    }
    super::apply_binary_to(&next.join(relative_exe), binary)?;
    // All fallible preparation, including signing, happens before replacement.
    sign(&next)?;
    swap_bundles(&next, bundle)?;
    // The old bundle is now in our staging directory, outside the new seal.
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn sign_and_verify(bundle: &Path) -> Result<(), String> {
    for args in [
        vec!["--force", "--deep", "--sign", "-"],
        vec!["--verify", "--deep", "--strict"],
    ] {
        let result = Command::new("/usr/bin/codesign")
            .args(args)
            .arg(bundle)
            .output()
            .map_err(|e| format!("应用签名工具无法运行：{e}"))?;
        if !result.status.success() {
            return Err("新应用签名或验证失败，原应用未修改；请下载完整应用包".into());
        }
    }
    Ok(())
}

fn swap_bundles(a: &Path, b: &Path) -> Result<(), String> {
    extern "C" {
        fn renamex_np(a: *const std::ffi::c_char, b: *const std::ffi::c_char, flags: u32) -> i32;
    }
    let a = CString::new(a.as_os_str().as_bytes()).map_err(|_| "更新路径无效")?;
    let b = CString::new(b.as_os_str().as_bytes()).map_err(|_| "应用路径无效")?;
    // macOS sys/stdio.h: RENAME_SWAP = 0x00000002. Both NUL-terminated paths
    // stay alive throughout this synchronous call; the original survives errors.
    if unsafe { renamex_np(a.as_ptr(), b.as_ptr(), 2) } != 0 {
        return Err(format!(
            "原子替换应用包失败，原应用未修改：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Stage, std::path::PathBuf, std::path::PathBuf) {
        let root =
            Stage(std::env::temp_dir().join(format!("neko-bundle-test-{}", uuid::Uuid::new_v4())));
        let bundle = root.0.join("Test App.app");
        let exe = bundle.join("Contents/MacOS/tool");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>tool</string><key>CFBundleIdentifier</key><string>local.neko.update-test</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
        std::fs::copy("/usr/bin/true", &exe).unwrap();
        sign_and_verify(&bundle).unwrap();
        (root, bundle, exe)
    }
    #[test]
    fn bundle_update_keeps_a_valid_seal_and_cleans_the_old_bundle() {
        let (root, bundle, exe) = fixture();
        let bytes = std::fs::read("/usr/bin/false").unwrap();
        apply(&bundle, &exe, &bytes).unwrap();
        assert!(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&bundle)
            .status()
            .unwrap()
            .success());
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
        assert!(!exe.with_file_name("tool.new").exists());
    }
    #[test]
    fn failed_signing_leaves_original_bytes_and_signature_untouched() {
        let (root, bundle, exe) = fixture();
        let original = std::fs::read(&exe).unwrap();
        let bytes = std::fs::read("/usr/bin/false").unwrap();
        let error = stage_and_apply(&bundle, &exe, &bytes, |_| {
            Err("synthetic signer failure".into())
        })
        .unwrap_err();
        assert!(error.contains("synthetic"));
        assert_eq!(std::fs::read(&exe).unwrap(), original);
        assert!(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&bundle)
            .status()
            .unwrap()
            .success());
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    }
}
