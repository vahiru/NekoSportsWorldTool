//! 状态层通用行为：版本戳、逐级迁移、损坏备份、权限、原子写入。
//! 各真实状态文件的格式夹具测试见 `crate::api::model::state_file_tests`。

use super::*;
use serde_json::json;

/// 测试专用数据目录：注入 `platform::data_dir()`，析构时清理。
pub(crate) struct TempDataDir(pub PathBuf);

impl TempDataDir {
    pub(crate) fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("neko-state-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(dir.clone()));
        Self(dir)
    }

    /// 目录中以 `<file>.corrupt-` 开头的备份文件。
    pub(crate) fn backups(&self, file: &str) -> Vec<PathBuf> {
        let prefix = format!("{file}.corrupt-");
        let mut out: Vec<_> = std::fs::read_dir(&self.0)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix)
            })
            .collect();
        out.sort();
        out
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = None);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
pub(crate) fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// v0：`{"title": .., "count": "<字符串>"}`
/// v1：`title` 改名 `name`
/// v2：`count` 改为数字
/// v3：新增 `tags`（缺省空数组）
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Demo {
    name: String,
    count: u32,
    tags: Vec<String>,
}

impl StateFile for Demo {
    const FILE_NAME: &'static str = "demo.json";
    const CURRENT_VERSION: u32 = 3;
    const SECRET: bool = true;

    fn migrate(from: u32, mut value: Value) -> Result<Value, String> {
        let map = value.as_object_mut().ok_or("期望对象")?;
        match from {
            0 => {
                let title = map.shift_remove("title").ok_or("缺少 title")?;
                map.insert("name".into(), title);
            }
            1 => {
                let count = map
                    .get("count")
                    .and_then(Value::as_str)
                    .ok_or("count 应为字符串")?;
                let count: u32 = count.parse().map_err(|e| format!("count: {e}"))?;
                map.insert("count".into(), count.into());
            }
            2 => {
                map.entry("tags").or_insert_with(|| json!([]));
            }
            _ => unreachable!("migrate is only called below CURRENT_VERSION"),
        }
        Ok(value)
    }
}

/// 非 secret、无迁移的最小类型。
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Plain {
    value: i64,
}

impl StateFile for Plain {
    const FILE_NAME: &'static str = "plain.json";
    const CURRENT_VERSION: u32 = 1;
    const SECRET: bool = false;
}

fn write_raw<T: StateFile>(bytes: impl AsRef<[u8]>) {
    std::fs::write(path::<T>(), bytes).unwrap();
}

fn read_value<T: StateFile>() -> Value {
    serde_json::from_slice(&std::fs::read(path::<T>()).unwrap()).unwrap()
}

#[test]
fn missing_file_is_none_and_remove_is_idempotent() {
    let _dir = TempDataDir::new("missing");
    assert!(read::<Demo>().unwrap().is_none());
    assert!(load::<Demo>().unwrap().is_none());
    remove::<Demo>().unwrap();
}

#[test]
fn unversioned_file_is_v0_and_runs_every_migration_step_in_order() {
    let _dir = TempDataDir::new("v0");
    let raw = br#"{"title":"old","count":"5"}"#;
    let (demo, stored) = decode::<Demo>(raw).unwrap();
    assert_eq!(stored, 0);
    assert_eq!(
        demo,
        Demo {
            name: "old".into(),
            count: 5,
            tags: vec![]
        }
    );

    write_raw::<Demo>(raw);
    let loaded = load::<Demo>().unwrap().unwrap();
    assert_eq!(loaded, demo);
    assert_eq!(
        std::fs::read(path::<Demo>()).unwrap(),
        raw,
        "loading never rewrites the file; the version is stamped on the next save"
    );
    save(&loaded).unwrap();
    let saved = read_value::<Demo>();
    assert_eq!(
        saved,
        json!({"version": 3, "name": "old", "count": 5, "tags": []})
    );
    assert_eq!(
        saved.as_object().unwrap().keys().next().map(String::as_str),
        Some(VERSION_KEY),
        "version is the first key"
    );
    assert_eq!(load::<Demo>().unwrap().unwrap(), demo);
}

#[test]
fn migration_starts_at_the_stored_version() {
    // v1 已改名但 count 仍是字符串：只应执行 1→2、2→3。
    let (demo, stored) = decode::<Demo>(br#"{"version":1,"name":"mid","count":"7"}"#).unwrap();
    assert_eq!(stored, 1);
    assert_eq!(demo.count, 7);
    assert_eq!(demo.name, "mid");
    // 当前版本不经过任何迁移（v2 的 count 字符串转换不会再跑）。
    let (demo, stored) =
        decode::<Demo>(br#"{"version":3,"name":"new","count":9,"tags":["a"]}"#).unwrap();
    assert_eq!(stored, 3);
    assert_eq!(demo.tags, vec!["a".to_string()]);
}

#[test]
fn newer_version_is_read_best_effort() {
    let (demo, stored) =
        decode::<Demo>(br#"{"version":9,"name":"future","count":1,"tags":[],"extra":true}"#)
            .unwrap();
    assert_eq!(stored, 9);
    assert_eq!(demo.name, "future");
}

#[test]
fn invalid_version_or_failed_migration_is_corrupt() {
    for raw in [
        &br#"{"version":"1","name":"x","count":1,"tags":[]}"#[..],
        br#"{"version":-1,"name":"x","count":1,"tags":[]}"#,
        br#"{"version":4294967296,"name":"x","count":1,"tags":[]}"#,
        // 迁移失败：v0 缺少 title
        br#"{"count":"1"}"#,
        // 迁移后类型不符
        br#"{"version":3,"name":"x","count":"oops","tags":[]}"#,
        b"not json",
        b"",
    ] {
        assert!(
            decode::<Demo>(raw).is_err(),
            "{}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn read_reports_corruption_without_touching_anything() {
    let dir = TempDataDir::new("read-corrupt");
    write_raw::<Demo>(b"{\"name\":\"trunc");
    let err = read::<Demo>().unwrap_err();
    assert!(
        matches!(err, StateError::Corrupt { backup: None, .. }),
        "{err}"
    );
    assert_eq!(std::fs::read(path::<Demo>()).unwrap(), b"{\"name\":\"trunc");
    assert!(dir.backups(Demo::FILE_NAME).is_empty());
}

#[test]
fn load_backs_up_corrupt_file_once_and_surfaces_the_error() {
    let dir = TempDataDir::new("load-corrupt");
    let garbage = b"{\"version\":3,\"name\":\"trunc";
    write_raw::<Demo>(garbage);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path::<Demo>(), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let err = load::<Demo>().unwrap_err();
    let StateError::Corrupt {
        backup: Some(backup),
        backup_error: None,
        ..
    } = &err
    else {
        panic!("expected a backed-up corruption error, got {err:?}");
    };
    assert!(err.to_string().contains(&backup.display().to_string()));
    assert_eq!(dir.backups(Demo::FILE_NAME), vec![backup.clone()]);
    assert_eq!(
        std::fs::read(backup).unwrap(),
        garbage,
        "backup keeps original bytes"
    );
    assert!(!path::<Demo>().exists(), "corrupt original moved aside");
    #[cfg(unix)]
    assert_eq!(mode(backup), 0o600, "secret backups are private");

    // 之后视为文件不存在，不会重复备份；保存后恢复正常。
    assert!(load::<Demo>().unwrap().is_none());
    assert_eq!(dir.backups(Demo::FILE_NAME).len(), 1);
    save(&Demo::default()).unwrap();
    assert_eq!(load::<Demo>().unwrap().unwrap(), Demo::default());

    // 再次损坏（同一毫秒内也不会覆盖旧备份）。
    write_raw::<Demo>(b"[]");
    assert!(load::<Demo>().is_err());
    let backups = dir.backups(Demo::FILE_NAME);
    assert_eq!(backups.len(), 2);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), garbage);
}

#[test]
fn backup_paths_never_collide() {
    let dir = TempDataDir::new("collide");
    let target = dir.0.join("x.json");
    let first = backup_by_copy(&target, b"1", false).unwrap();
    let second = backup_by_copy(&target, b"2", false).unwrap();
    let third = backup_by_copy(&target, b"3", false).unwrap();
    assert_ne!(first, second);
    assert_ne!(second, third);
    assert_eq!(std::fs::read(first).unwrap(), b"1");
    assert_eq!(std::fs::read(second).unwrap(), b"2");
    assert_eq!(std::fs::read(third).unwrap(), b"3");
}

#[test]
fn save_is_atomic_and_secret_files_are_private() {
    let dir = TempDataDir::new("save");
    // 预先存在、权限宽松的文件会被替换为 0600 的新文件。
    write_raw::<Demo>(b"{}");
    save(&Demo {
        name: "n".into(),
        count: 1,
        tags: vec![],
    })
    .unwrap();
    save(&Plain { value: 3 }).unwrap();
    assert_eq!(read_value::<Plain>(), json!({"version": 1, "value": 3}));
    let leftovers: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    #[cfg(unix)]
    assert_eq!(mode(&path::<Demo>()), 0o600);
}

#[test]
fn encode_rejects_non_objects_and_the_reserved_key() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct List(Vec<u32>);
    impl StateFile for List {
        const FILE_NAME: &'static str = "list.json";
        const CURRENT_VERSION: u32 = 1;
        const SECRET: bool = false;
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Clash {
        version: u32,
    }
    impl StateFile for Clash {
        const FILE_NAME: &'static str = "clash.json";
        const CURRENT_VERSION: u32 = 1;
        const SECRET: bool = false;
    }
    assert!(matches!(
        encode(&List(vec![1])),
        Err(StateError::Encode { .. })
    ));
    assert!(matches!(
        encode(&Clash { version: 1 }),
        Err(StateError::Encode { .. })
    ));
}

#[test]
fn unreadable_path_is_an_io_error_not_corruption() {
    let _dir = TempDataDir::new("io");
    // 目录占据文件名：读取失败但不是“内容损坏”，也不做备份。
    std::fs::create_dir_all(path::<Plain>()).unwrap();
    assert!(matches!(load::<Plain>(), Err(StateError::Io { .. })));
    assert!(path::<Plain>().is_dir());
}
