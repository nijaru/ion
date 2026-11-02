//! One bounded operator-file snapshot; decode only sections the recipe uses.
use anyhow::{Context, Result};
use serde::{
    Deserialize,
    de::{DeserializeOwned, IgnoredAny},
};
use std::{io, path::Path};

pub(crate) const MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("invalid {section} settings.json at line {line}, column {column}")]
pub(crate) struct InvalidSettings {
    section: &'static str,
    line: usize,
    column: usize,
}
/// A bounded operator configuration snapshot. Unused recipe sections stay opaque.
pub struct UserSettings {
    bytes: Vec<u8>,
}
impl Default for UserSettings {
    fn default() -> Self {
        Self {
            bytes: b"{}".to_vec(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Sections<S: Default, Q: Default, V: Default, T: Default = IgnoredAny> {
    shell: S,
    search: Q,
    tool_veto: V,
    theme: T,
}
impl UserSettings {
    pub fn load(root: &Path) -> Result<Self> {
        Self::read(&root.join("settings.json"))
    }

    fn read(path: &Path) -> Result<Self> {
        let settings = match crate::file_io::read_bounded(path, MAX_BYTES) {
            Ok(bytes) => Self { bytes },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        settings.decode::<Sections<IgnoredAny, IgnoredAny, IgnoredAny>>("user")?;
        Ok(settings)
    }
    pub(crate) fn shell(&self) -> Result<crate::command_environment::ShellSettings> {
        Ok(self
            .decode::<Sections<crate::command_environment::ShellSettings, IgnoredAny, IgnoredAny>>(
                "shell",
            )?
            .shell)
    }
    pub(crate) fn search(&self) -> Result<crate::SearchSettings> {
        Ok(self
            .decode::<Sections<IgnoredAny, crate::SearchSettings, IgnoredAny>>("search")?
            .search)
    }
    pub(crate) fn tool_veto(&self) -> Result<Option<crate::ToolVetoSettings>> {
        Ok(self
            .decode::<Sections<IgnoredAny, IgnoredAny, Option<crate::ToolVetoSettings>>>(
                "tool veto",
            )?
            .tool_veto)
    }
    /// Theme names are presentation data; App owns their meaning and validation.
    pub fn theme(&self) -> Result<Option<String>> {
        Ok(self
            .decode::<Sections<IgnoredAny, IgnoredAny, IgnoredAny, Option<String>>>("theme")?
            .theme)
    }

    /// Patch only the preference in a fresh document under the cooperating
    /// writer lock. External editors do not participate in this lock.
    /// Success means the preference was published. An optional warning reports
    /// that directory sync failed afterward, so crash durability is uncertain.
    pub fn save_theme(root: &Path, theme: &str) -> Result<Option<String>> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        let mutation = crate::file_io::FileMutation::try_open(&root.join("settings.json"))?;
        let fresh = Self::read(mutation.path())?;
        let mut document: serde_json::Value = fresh.decode("user")?;
        document
            .as_object_mut()
            .expect("validated settings object")
            .insert("theme".into(), serde_json::Value::String(theme.into()));
        let mut bytes = serde_json::to_vec_pretty(&document)?;
        bytes.push(b'\n');
        anyhow::ensure!(
            bytes.len() <= MAX_BYTES,
            "settings.json exceeds its byte bound"
        );
        let permissions = match std::fs::metadata(mutation.path()) {
            Ok(metadata) => {
                mutation.ensure_writable()?;
                metadata.permissions()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::Permissions::from_mode(0o600)
            }
            Err(error) => return Err(error.into()),
        };
        match mutation.replace(&bytes, Some(permissions)) {
            Ok(()) => Ok(None),
            Err(error)
                if error
                    .get_ref()
                    .is_some_and(|cause| cause.is::<crate::file_io::PublicationSyncError>()) =>
            {
                Ok(Some(error.to_string()))
            }
            Err(error) => Err(error).context("save theme preference"),
        }
    }

    fn decode<T: DeserializeOwned>(&self, section: &'static str) -> Result<T> {
        // Parse the captured bytes, not a Value clone: retain genuine positions
        // without printing Serde's invalid value (which can be private code/keys).
        serde_json::from_slice(&self.bytes).map_err(|error| {
            InvalidSettings {
                section,
                line: error.line(),
                column: error.column(),
            }
            .into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_saves_patch_fresh_data_and_fail_promptly_behind_an_owned_writer() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("ion-theme-save-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(&path, r#"{"search":{"native":false},"theme":"dark"}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let captured = UserSettings::load(&root).unwrap();
        let lock = crate::file_io::FileMutation::try_open(&path).unwrap();
        let before = std::time::Instant::now();
        assert!(UserSettings::save_theme(&root, "light").is_err());
        assert!(before.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(
            UserSettings::load(&root)
                .unwrap()
                .theme()
                .unwrap()
                .as_deref(),
            Some("dark")
        );
        // A cooperating writer publishes a newer unrelated setting after capture.
        lock.replace(br#"{"search":{"native":true},"tool_veto":{"program":"PRIVATE_PROGRAM"},"theme":"dark"}"#, Some(std::fs::Permissions::from_mode(0o640))).unwrap();
        drop(lock);
        UserSettings::save_theme(&root, "light").unwrap();
        let fresh = UserSettings::load(&root).unwrap();
        assert_eq!(captured.theme().unwrap().as_deref(), Some("dark"));
        assert_eq!(fresh.theme().unwrap().as_deref(), Some("light"));
        assert!(fresh.search().unwrap().native);
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["tool_veto"]["program"], "PRIVATE_PROGRAM");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        std::fs::write(&path, r#"{"theme":{"bad":"PRIVATE_VALUE"}}"#).unwrap();
        let unused = UserSettings::load(&root).unwrap();
        assert!(unused.search().is_ok());
        let error = unused.theme().unwrap_err().to_string();
        assert!(!error.contains("PRIVATE_VALUE"));
        assert!(error.contains("line 1"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn captured_sections_are_stable_and_unused_sections_are_not_interpreted() {
        let root = std::env::temp_dir().join(format!("ion-settings-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(&path,r#"{"shell":{"path":"PRIVATE_INVALID_VALUE","extra":"PRIVATE_INVALID_VALUE"},"search":{"native":false,"fallbacks":[]},"tool_veto":{"program":"PRIVATE_INVALID_VALUE","timeout_ms":"PRIVATE_INVALID_VALUE"}}"#).unwrap();
        let captured = UserSettings::load(&root).unwrap();
        std::fs::write(&path, r#"{"search":{"native":true}}"#).unwrap();
        assert!(!captured.search().unwrap().native);
        assert!(UserSettings::load(&root).unwrap().search().unwrap().native);
        let veto_error = captured.tool_veto().err().unwrap().to_string();
        assert!(veto_error.contains("line 1") && !veto_error.contains("PRIVATE_INVALID_VALUE"));
        let error = captured.shell().err().unwrap().to_string();
        assert!(
            error.contains("line 1") && !error.contains("PRIVATE_INVALID_VALUE"),
            "{error}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
