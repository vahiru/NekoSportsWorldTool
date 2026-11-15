//! Multi-account slots adapted from ZhYue314's bfe2db1 / 3c7311c.
//! The original account keeps the legacy files byte-for-byte. New accounts have
//! private, immutable UUID directories; public school road caches stay shared.
use crate::state::{self, StateFile};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const ORIGINAL: &str = "default";

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Profiles {
    pub active: String,
    pub entries: Vec<Profile>,
    #[serde(skip)]
    pub environment_repairs: usize,
}

impl Default for Profiles {
    fn default() -> Self {
        Self {
            environment_repairs: 0,
            active: ORIGINAL.into(),
            entries: vec![Profile {
                id: ORIGINAL.into(),
                name: "原有账号".into(),
            }],
        }
    }
}

impl StateFile for Profiles {
    const FILE_NAME: &'static str = "profiles.json";
    const CURRENT_VERSION: u32 = 1;
    const SECRET: bool = true;
}

impl Profiles {
    pub fn load() -> Result<Self, String> {
        let _scope = state::enter(None);
        // Do not silently replace a corrupt index: that could hide saved accounts.
        let mut index = state::read::<Self>()?.unwrap_or_default();
        let mut ids = std::collections::HashSet::new();
        if !index.entries.iter().all(|p| {
            (p.id == ORIGINAL || (p.id.len() == 32 && p.id.bytes().all(|b| b.is_ascii_hexdigit())))
                && !p.name.trim().is_empty()
                && ids.insert(&p.id)
        }) || !ids.contains(&index.active)
            || !ids.contains(&ORIGINAL.to_owned())
        {
            return Err("账号索引无效，请恢复 profiles.json 的备份".into());
        }
        index.environment_repairs = index.repair_duplicate_environments()?;
        Ok(index)
    }

    fn save(&self) -> Result<(), String> {
        let _scope = state::enter(None);
        state::save(self).map_err(Into::into)
    }

    #[cfg(any(target_os = "android", test))]
    pub fn is_original(&self) -> bool {
        self.active == ORIGINAL
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.root_for(&self.active)
    }

    fn root_for(&self, id: &str) -> Option<PathBuf> {
        (id != ORIGINAL).then(|| crate::platform::data_dir().join("accounts").join(id))
    }

    pub fn name(&self) -> &str {
        self.entries
            .iter()
            .find(|p| p.id == self.active)
            .map(|p| p.name.as_str())
            .unwrap_or("原有账号")
    }

    pub fn select(&mut self, id: &str) -> Result<(), String> {
        if !self.entries.iter().any(|p| p.id == id) {
            return Err("找不到该账号".into());
        }
        let mut next = self.clone();
        next.active = id.into();
        next.save()?;
        *self = next;
        Ok(())
    }

    pub fn rename(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("请输入账号名称".into());
        }
        let mut next = self.clone();
        next.entries
            .iter_mut()
            .find(|p| p.id == next.active)
            .ok_or("找不到当前账号")?
            .name = name.into();
        next.save()?;
        *self = next;
        Ok(())
    }

    pub fn create(
        &mut self,
        name: &str,
        identity: &crate::api::model::HeaderIdentity,
        config: &crate::api::model::Config,
    ) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("请输入账号名称".into());
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let root = self.root_for(&id).unwrap();
        std::fs::create_dir_all(root.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let result = (|| {
            let _scope = state::enter(Some(root.clone()));
            // Physical identifiers are generated once and persisted for this slot.
            // Real location is copied as a starting value, then independently editable.
            state::save(&identity.independent_device(&identity.platform))?;
            let mut config = config.clone();
            config.username.clear();
            config.password.clear();
            config.school_campus = None;
            state::save(&config).map_err(String::from)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        let mut next = self.clone();
        next.entries.push(Profile {
            id: id.clone(),
            name: name.into(),
        });
        if let Err(error) = next.save() {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        *self = next;
        Ok(id)
    }

    /// Upgrade slots made by the previous release, which copied the device ID.
    /// Always keep the original account, even if it is not first in the index.
    fn repair_duplicate_environments(&self) -> Result<usize, String> {
        use crate::api::model::{HeaderIdentity, Session};
        let mut entries: Vec<_> = self.entries.iter().collect();
        entries.sort_by_key(|p| p.id != ORIGINAL);
        let mut seen = std::collections::HashSet::new();
        let mut repaired = 0;
        for profile in entries {
            let _scope = state::enter(self.root_for(&profile.id));
            let Some(identity) = state::read::<HeaderIdentity>()? else {
                continue;
            };
            let device_id = identity.device_id.trim().to_ascii_lowercase();
            if device_id.is_empty() || seen.insert(device_id) {
                continue;
            }
            let replacement = identity.independent_device(&identity.platform);
            let identity_path = state::path::<HeaderIdentity>();
            let session_path = state::path::<Session>();
            let identity_bytes = std::fs::read(&identity_path).map_err(|e| e.to_string())?;
            let session_bytes = match std::fs::read(&session_path) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.to_string()),
            };
            let backup = identity_path
                .parent()
                .unwrap()
                .join("environment-backups")
                .join(uuid::Uuid::new_v4().simple().to_string());
            std::fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                for dir in [&backup, backup.parent().unwrap()] {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                        .map_err(|e| e.to_string())?;
                }
            }
            state::write_atomic(&backup.join("identity.json"), &identity_bytes, true)
                .map_err(|e| e.to_string())?;
            if let Some(bytes) = &session_bytes {
                state::write_atomic(&backup.join("session.json"), bytes, true)
                    .map_err(|e| e.to_string())?;
            }
            // Remove the old session first: even an interrupted upgrade must not
            // pair a new identity with a token bound to the old device.
            state::remove::<Session>().map_err(|e| e.to_string())?;
            if let Err(error) = state::save(&replacement) {
                if let Some(bytes) = &session_bytes {
                    state::write_atomic(&session_path, bytes, true).map_err(|restore| {
                        format!(
                            "{error}；旧会话恢复失败：{restore}；备份在 {}",
                            backup.display()
                        )
                    })?;
                }
                return Err(error.into());
            }
            seen.insert(replacement.device_id.to_ascii_lowercase());
            repaired += 1;
        }
        Ok(repaired)
    }

    /// Remove from the picker, retaining files for recovery and late workers.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if id == ORIGINAL || id == self.active {
            return Err("请先切换账号；原有账号不能移除".into());
        }
        let mut next = self.clone();
        next.entries.retain(|p| p.id != id);
        next.save()?;
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{self, Config, HeaderIdentity, Session};

    struct DataDir(PathBuf);
    impl DataDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("neko-profiles-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            crate::platform::TEST_DATA_DIR.with(|p| p.replace(Some(path.clone())));
            Self(path)
        }
    }
    impl Drop for DataDir {
        fn drop(&mut self) {
            crate::platform::TEST_DATA_DIR.with(|p| p.replace(None));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn original_files_survive_creation_and_restart_without_migration() {
        let dir = DataDir::new();
        let identity = HeaderIdentity {
            device_id: "original-device".into(),
            ..Default::default()
        };
        let config = Config {
            username: "original-user".into(),
            password: "original-password".into(),
            remember: true,
            ..Default::default()
        };
        let session = Session {
            uid: 7,
            token: "original-token".into(),
            ..Default::default()
        };
        state::save(&identity).unwrap();
        state::save(&session).unwrap();
        state::save(&config).unwrap();
        let before: Vec<_> = ["identity.json", "session.json", "config.json"]
            .map(|f| std::fs::read(dir.0.join(f)).unwrap())
            .into();
        let mut profiles = Profiles::load().unwrap();
        let id = profiles
            .create("../../new-account", &identity, &config)
            .unwrap();
        profiles.select(&id).unwrap();
        assert_eq!(Profiles::load().unwrap().active, id);
        {
            let _scope = state::enter(profiles.root());
            assert_ne!(model::load_identity().device_id, "original-device");
            assert!(model::load_config().username.is_empty());
            assert!(model::load_config().password.is_empty());
            assert!(!model::load_session().is_logged_in());
            model::save_session(&Session {
                uid: 8,
                token: "new-token".into(),
                ..Default::default()
            })
            .unwrap();
        }
        profiles.select(ORIGINAL).unwrap();
        let after: Vec<_> = ["identity.json", "session.json", "config.json"]
            .map(|f| std::fs::read(dir.0.join(f)).unwrap())
            .into();
        assert_eq!(before, after);
        assert_eq!(model::load_session().uid, 7);
        assert!(dir
            .0
            .join("accounts")
            .join(&id)
            .join("session.json")
            .exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in ["identity.json", "config.json", "session.json"] {
                assert_eq!(
                    std::fs::metadata(dir.0.join("accounts").join(&id).join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn each_profile_gets_a_distinct_persistent_environment_on_both_platforms() {
        for platform in ["ios", "android"] {
            let _dir = DataDir::new();
            let mut profiles = Profiles::default();
            assert!(profiles.is_original());
            let mut source = HeaderIdentity::default().independent_device(platform);
            source.city = "Test city".into();
            source.anchor_lat = 39.91;
            source.anchor_lon = 116.41;
            state::save(&source).unwrap();
            let first = profiles
                .create("First", &source, &Config::default())
                .unwrap();
            let second = profiles
                .create("Second", &source, &Config::default())
                .unwrap();
            let mut devices = std::collections::HashSet::from([source.device_id.clone()]);
            let mut macs = std::collections::HashSet::from([source.mac_address.clone()]);
            let mut idfas = std::collections::HashSet::from([source.idfa.clone()]);
            for id in [&first, &second] {
                profiles.select(id).unwrap();
                assert!(!profiles.is_original());
                let _scope = state::enter(profiles.root());
                let identity = model::load_identity();
                assert!(devices.insert(identity.device_id.clone()));
                assert!(macs.insert(identity.mac_address.clone()));
                assert_eq!(identity.platform, platform);
                assert_ne!(identity.os_version, source.os_version);
                if platform == "ios" {
                    assert!(idfas.insert(identity.idfa.clone()));
                } else {
                    assert_ne!(identity.device_name, source.device_name);
                }
                assert_eq!(identity.city, source.city);
                assert_eq!(
                    (identity.anchor_lat, identity.anchor_lon),
                    (source.anchor_lat, source.anchor_lon)
                );
                assert_eq!(
                    serde_json::to_value(&identity).unwrap(),
                    serde_json::to_value(model::load_identity()).unwrap()
                );
                let reloaded = Profiles::load().unwrap();
                assert_eq!(reloaded.environment_repairs, 0);
                assert_eq!(reloaded.active, *id);
            }
        }
    }

    #[test]
    fn legacy_duplicate_is_backed_up_once_and_logged_out_without_changing_original() {
        let dir = DataDir::new();
        let source = HeaderIdentity::default().independent_device("ios");
        state::save(&source).unwrap();
        state::save(&Session {
            uid: 7,
            token: "original-token".into(),
            ..Default::default()
        })
        .unwrap();
        let mut profiles = Profiles::default();
        let id = profiles
            .create("Legacy", &source, &Config::default())
            .unwrap();
        profiles.select(&id).unwrap();
        profiles.entries.reverse();
        profiles.save().unwrap();
        let root = profiles.root().unwrap();
        {
            let _scope = state::enter(Some(root.clone()));
            state::save(&source).unwrap();
            state::save(&Session {
                uid: 8,
                token: "legacy-token".into(),
                ..Default::default()
            })
            .unwrap();
        }
        let old_identity = std::fs::read(root.join("identity.json")).unwrap();
        let old_session = std::fs::read(root.join("session.json")).unwrap();
        let old_config = std::fs::read(root.join("config.json")).unwrap();
        let original_identity = std::fs::read(dir.0.join("identity.json")).unwrap();
        let original_session = std::fs::read(dir.0.join("session.json")).unwrap();
        let loaded = Profiles::load().unwrap();
        assert_eq!(loaded.environment_repairs, 1);
        assert_eq!(loaded.active, id);
        assert_eq!(
            std::fs::read(dir.0.join("identity.json")).unwrap(),
            original_identity
        );
        assert_eq!(
            std::fs::read(dir.0.join("session.json")).unwrap(),
            original_session
        );
        assert_eq!(std::fs::read(root.join("config.json")).unwrap(), old_config);
        let new_identity = std::fs::read(root.join("identity.json")).unwrap();
        assert_ne!(new_identity, old_identity);
        assert!(!root.join("session.json").exists());
        let backups: Vec<_> = std::fs::read_dir(root.join("environment-backups"))
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read(backups[0].join("identity.json")).unwrap(),
            old_identity
        );
        assert_eq!(
            std::fs::read(backups[0].join("session.json")).unwrap(),
            old_session
        );
        assert_eq!(Profiles::load().unwrap().environment_repairs, 0);
        assert_eq!(
            std::fs::read(root.join("identity.json")).unwrap(),
            new_identity
        );
        assert_eq!(
            std::fs::read_dir(root.join("environment-backups"))
                .unwrap()
                .count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(backups[0].join("session.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_environment_backup_keeps_both_identity_and_session() {
        let _dir = DataDir::new();
        let source = HeaderIdentity::default().independent_device("ios");
        state::save(&source).unwrap();
        let mut profiles = Profiles::default();
        let id = profiles
            .create("Legacy", &source, &Config::default())
            .unwrap();
        profiles.select(&id).unwrap();
        let root = profiles.root().unwrap();
        {
            let _scope = state::enter(Some(root.clone()));
            state::save(&source).unwrap();
            state::save(&Session {
                uid: 8,
                token: "legacy-token".into(),
                ..Default::default()
            })
            .unwrap();
        }
        let old_identity = std::fs::read(root.join("identity.json")).unwrap();
        let old_session = std::fs::read(root.join("session.json")).unwrap();
        std::fs::write(root.join("environment-backups"), b"blocked").unwrap();
        assert!(Profiles::load().is_err());
        assert_eq!(
            std::fs::read(root.join("identity.json")).unwrap(),
            old_identity
        );
        assert_eq!(
            std::fs::read(root.join("session.json")).unwrap(),
            old_session
        );
    }

    #[test]
    fn nested_scopes_restore_even_when_unwinding() {
        let dir = DataDir::new();
        let first = dir.0.join("first");
        let second = dir.0.join("second");
        let _scope = state::enter(Some(first.clone()));
        assert_eq!(state::path::<Config>(), first.join("config.json"));
        let _ = std::panic::catch_unwind(|| {
            let _scope = state::enter(Some(second));
            panic!("test unwind");
        });
        assert_eq!(state::path::<Config>(), first.join("config.json"));
    }

    #[test]
    fn invalid_index_and_failed_writes_keep_existing_accounts() {
        let dir = DataDir::new();
        for active in ["../escape", "missing"] {
            let raw = serde_json::json!({"active":active,"entries":[{"id":"default","name":"Original"},{"id":"../escape","name":"Bad"}]});
            std::fs::write(dir.0.join("profiles.json"), raw.to_string()).unwrap();
            assert!(Profiles::load().is_err());
            assert_eq!(
                std::fs::read_to_string(dir.0.join("profiles.json")).unwrap(),
                raw.to_string()
            );
        }
        std::fs::remove_file(dir.0.join("profiles.json")).unwrap();
        std::fs::create_dir(dir.0.join("profiles.json")).unwrap();
        let mut profiles = Profiles::default();
        assert!(profiles.rename("Renamed").is_err());
        assert_eq!(profiles.name(), "原有账号");
        assert!(profiles
            .create("New", &HeaderIdentity::default(), &Config::default())
            .is_err());
        assert!(std::fs::read_dir(dir.0.join("accounts"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn removal_retains_recovery_files_and_protects_active_profile() {
        let _dir = DataDir::new();
        let mut profiles = Profiles::default();
        let id = profiles
            .create("New", &HeaderIdentity::default(), &Config::default())
            .unwrap();
        profiles.select(&id).unwrap();
        assert!(profiles.remove(&id).is_err());
        assert!(profiles.remove(ORIGINAL).is_err());
        let root = profiles.root().unwrap();
        profiles.select(ORIGINAL).unwrap();
        profiles.remove(&id).unwrap();
        assert_eq!(Profiles::load().unwrap().entries.len(), 1);
        assert!(root.join("identity.json").exists());
    }
}
