//! One bounded operator-file snapshot; decode only sections the recipe uses.
use anyhow::{Context, Result};
use serde::{
    Deserialize,
    de::{DeserializeOwned, IgnoredAny},
};
use std::{io, path::Path};

pub(crate) const MAX_BYTES: usize = 64 * 1024;
pub(crate) struct UserSettings {
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
struct Sections<S: Default, Q: Default> {
    shell: S,
    search: Q,
}
impl UserSettings {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let path = root.join("settings.json");
        let settings = match crate::file_io::read_bounded(&path, MAX_BYTES) {
            Ok(bytes) => Self { bytes },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        settings.decode::<Sections<IgnoredAny, IgnoredAny>>("user")?;
        Ok(settings)
    }
    pub(crate) fn shell(&self) -> Result<crate::command_environment::ShellSettings> {
        Ok(self
            .decode::<Sections<crate::command_environment::ShellSettings, IgnoredAny>>("shell")?
            .shell)
    }
    pub(crate) fn search(&self) -> Result<crate::SearchSettings> {
        Ok(self
            .decode::<Sections<IgnoredAny, crate::SearchSettings>>("search")?
            .search)
    }
    fn decode<T: DeserializeOwned>(&self, section: &str) -> Result<T> {
        // Parse the captured bytes, not a Value clone: retain genuine positions
        // without printing Serde's invalid value (which can be private code/keys).
        serde_json::from_slice(&self.bytes).map_err(|error| {
            anyhow::anyhow!(
                "invalid {section} settings.json at line {}, column {}",
                error.line(),
                error.column()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_sections_are_stable_and_unused_sections_are_not_interpreted() {
        let root = std::env::temp_dir().join(format!("ion-settings-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(&path,r#"{"shell":{"path":"PRIVATE_INVALID_VALUE","extra":"PRIVATE_INVALID_VALUE"},"search":{"native":false,"fallbacks":[]}}"#).unwrap();
        let captured = UserSettings::load(&root).unwrap();
        std::fs::write(&path, r#"{"search":{"native":true}}"#).unwrap();
        assert!(!captured.search().unwrap().native);
        assert!(UserSettings::load(&root).unwrap().search().unwrap().native);
        let error = captured.shell().err().unwrap().to_string();
        assert!(
            error.contains("line 1") && !error.contains("PRIVATE_INVALID_VALUE"),
            "{error}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
