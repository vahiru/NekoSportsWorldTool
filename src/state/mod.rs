//! 版本化的本地状态文件层。
//!
//! 所有小型 JSON 状态文件（config.json、session.json、identity.json、points_cache.json、
//! ai_sports.json）都经由这里读写：
//!
//! - 磁盘格式：保持原文件名与原 JSON 结构，仅在顶层对象最前面多一个 `"version": N`。
//!   没有 `version` 的旧文件视为 v0，读取时逐级调用 [`StateFile::migrate`] 升到当前版本，
//!   下次保存时写入当前版本号。旧版本程序读取新文件时会忽略多出的 `version` 字段。
//! - 写入：同目录临时文件 → fsync → rename（[`write_atomic`]）；密文件（凭证、设备指纹）
//!   在 Unix 下为 0600。
//! - 损坏：无法解析 / 迁移 / 反序列化的文件不会被静默丢弃。[`load`] 会把原文件改名为
//!   `<name>.corrupt-<ms>` 备份后返回错误；[`read`] 只报告错误，不动任何文件。
//! - 比当前程序更新的版本（降级运行）：尽力按当前结构读取，未知字段会在下次保存时丢失。
//!
//! 约束：状态类型顶层不得有名为 `version` 的字段（该键保留给版本号）。
//!
//! 提交回执日志（submission_receipts/*.jsonl）与学校路网缓存不走这里。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// 顶层保留的版本键。
pub const VERSION_KEY: &str = "version";

/// 一个由状态层管理的本地 JSON 文件。
pub trait StateFile: serde::Serialize + serde::de::DeserializeOwned {
    /// 数据目录下的文件名（保持历史文件名不变）。
    const FILE_NAME: &'static str;
    /// 当前写入的版本号；无版本号的旧文件为 0。
    const CURRENT_VERSION: u32;
    /// 含凭证或设备指纹：Unix 下仅本用户可读写（0600），损坏备份同样如此。
    const SECRET: bool;

    /// 把版本为 `from` 的内容升级一级到 `from + 1`（`version` 键已剥离）。
    /// 由 [`decode`] 对 `from = 磁盘版本 .. CURRENT_VERSION` 依次调用。
    fn migrate(from: u32, value: Value) -> Result<Value, String> {
        let _ = from;
        Ok(value)
    }
}

/// 状态文件读写错误。
#[derive(Debug)]
pub enum StateError {
    /// 文件存在但无法读取，或无法写入。
    Io {
        path: PathBuf,
        action: &'static str,
        error: std::io::Error,
    },
    /// 文件内容无法解析、迁移或反序列化。
    Corrupt {
        path: PathBuf,
        reason: String,
        /// [`load`] 备份成功时为备份路径；[`read`] 恒为 None。
        backup: Option<PathBuf>,
        /// 备份失败原因（此时原文件保持不动）。
        backup_error: Option<String>,
    },
    /// 值无法序列化为带版本号的 JSON 对象。
    Encode { name: &'static str, reason: String },
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                path,
                action,
                error,
            } => write!(f, "{action} {} 失败: {error}", path.display()),
            Self::Corrupt {
                path,
                reason,
                backup,
                backup_error,
            } => {
                write!(f, "{} 内容损坏: {reason}", path.display())?;
                if let Some(backup) = backup {
                    write!(f, "；原文件已备份为 {}", backup.display())?;
                }
                if let Some(e) = backup_error {
                    write!(f, "；备份失败（{e}），原文件未改动")?;
                }
                Ok(())
            }
            Self::Encode { name, reason } => write!(f, "序列化 {name} 失败: {reason}"),
        }
    }
}

impl std::error::Error for StateError {}

impl From<StateError> for String {
    fn from(e: StateError) -> Self {
        e.to_string()
    }
}

/// 状态文件的完整路径（平台数据目录下）。
pub fn path<T: StateFile>() -> PathBuf {
    crate::platform::data_dir().join(T::FILE_NAME)
}

/// 解析磁盘字节：剥离版本号 → 逐级迁移 → 反序列化。返回值与磁盘上的版本号。
pub fn decode<T: StateFile>(raw: &[u8]) -> Result<(T, u32), String> {
    let mut value: Value = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    let stored = match &mut value {
        Value::Object(map) => match map.shift_remove(VERSION_KEY) {
            None => 0,
            Some(v) => v
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| format!("无效的版本号 {v}"))?,
        },
        // 非对象（如旧版 ai_sports.json 的裸数组）只可能是 v0。
        _ => 0,
    };
    for from in stored..T::CURRENT_VERSION {
        value = T::migrate(from, value).map_err(|e| format!("从 v{from} 迁移失败: {e}"))?;
    }
    let decoded = serde_json::from_value(value).map_err(|e| e.to_string())?;
    Ok((decoded, stored))
}

/// 序列化为带 `"version": CURRENT_VERSION`（置于最前）的 pretty JSON。
pub fn encode<T: StateFile>(value: &T) -> Result<Vec<u8>, StateError> {
    let encode_err = |reason: String| StateError::Encode {
        name: T::FILE_NAME,
        reason,
    };
    let Value::Object(fields) =
        serde_json::to_value(value).map_err(|e| encode_err(e.to_string()))?
    else {
        return Err(encode_err("状态文件顶层必须是 JSON 对象".into()));
    };
    if fields.contains_key(VERSION_KEY) {
        return Err(encode_err(format!("顶层字段 `{VERSION_KEY}` 为保留键")));
    }
    let mut out = Map::with_capacity(fields.len() + 1);
    out.insert(VERSION_KEY.into(), T::CURRENT_VERSION.into());
    out.extend(fields);
    serde_json::to_vec_pretty(&Value::Object(out)).map_err(|e| encode_err(e.to_string()))
}

/// 读取但不产生任何副作用：Ok(None) 表示文件不存在；损坏时返回 [`StateError::Corrupt`]
/// （`backup` 为 None）且不改动文件。
pub fn read<T: StateFile>() -> Result<Option<T>, StateError> {
    let path = path::<T>();
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(StateError::Io {
                path,
                action: "读取",
                error,
            })
        }
    };
    decode::<T>(&raw)
        .map(|(value, _)| Some(value))
        .map_err(|reason| StateError::Corrupt {
            path,
            reason,
            backup: None,
            backup_error: None,
        })
}

/// 读取；文件损坏时先把原文件改名为 `<name>.corrupt-<ms>`（密文件收紧为 0600）再返回错误，
/// 因此同一份损坏内容只备份一次，之后的读取视为文件不存在。
pub fn load<T: StateFile>() -> Result<Option<T>, StateError> {
    match read::<T>() {
        Err(StateError::Corrupt { path, reason, .. }) => {
            let (backup, backup_error) = match backup_by_rename(&path, T::SECRET) {
                Ok(backup) => (Some(backup), None),
                Err(e) => (None, Some(e.to_string())),
            };
            Err(StateError::Corrupt {
                path,
                reason,
                backup,
                backup_error,
            })
        }
        other => other,
    }
}

/// 原子写入当前版本。
pub fn save<T: StateFile>(value: &T) -> Result<(), StateError> {
    let path = path::<T>();
    let bytes = encode(value)?;
    write_atomic(&path, &bytes, T::SECRET).map_err(|error| StateError::Io {
        path,
        action: "写入",
        error,
    })
}

/// 删除状态文件（不存在视为成功）。
pub fn remove<T: StateFile>() -> std::io::Result<()> {
    match std::fs::remove_file(path::<T>()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 同目录临时文件 → fsync → rename：中途断电/崩溃只会留下旧文件或新文件，不会截断。
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    use std::io::Write;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    // 目录项持久化（best effort；部分平台不支持打开目录）。
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(dir) = std::fs::File::open(dir) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// 损坏备份文件名：`<name>.corrupt-<ms>`，同毫秒冲突时追加 `-<n>`，绝不覆盖已有备份。
pub fn corrupt_backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".into());
    let base = format!("{name}.corrupt-{}", crate::crypto::envelope::now_ms());
    let mut candidate = path.with_file_name(&base);
    let mut n = 1;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{base}-{n}"));
        n += 1;
    }
    candidate
}

/// 把损坏文件的原始字节另存一份（原文件保留，交由调用方修复后覆盖）。
pub fn backup_by_copy(path: &Path, raw: &[u8], private: bool) -> std::io::Result<PathBuf> {
    let backup = corrupt_backup_path(path);
    write_atomic(&backup, raw, private)?;
    Ok(backup)
}

/// 把损坏文件原样改名为备份（rename 不复制也不丢字节）。
fn backup_by_rename(path: &Path, private: bool) -> std::io::Result<PathBuf> {
    let backup = corrupt_backup_path(path);
    std::fs::rename(path, &backup)?;
    // 已改名成功：收紧权限失败也不能报告为“原文件未改动”，因此 best effort。
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(backup)
}

#[cfg(test)]
pub(crate) mod tests;
