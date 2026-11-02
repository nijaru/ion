//! Synchronous preparation for startup or an owned native worker, never painting.
use super::{Selection, validate_name};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, io, path::PathBuf};

pub(crate) struct Files {
    root: PathBuf,
    selected: String,
}

pub(crate) struct Saved {
    pub(super) selection: Selection,
    pub(super) warning: Option<String>,
}

impl Files {
    pub(super) fn new(root: PathBuf, selected: String) -> Self {
        Self { root, selected }
    }

    pub(crate) fn load(&self, name: &str) -> Result<Selection> {
        Selection::load(&self.root, name)
    }

    pub(crate) fn save(&self, name: &str) -> Result<Saved> {
        let selection = self.load(name)?;
        let warning = ion_app::user_settings::UserSettings::save_theme(&self.root, name)?;
        Ok(Saved { selection, warning })
    }

    pub(crate) fn names(&self) -> Result<Vec<String>> {
        let mut names = vec!["system".into(), "dark".into(), "light".into()];
        let entries = match std::fs::read_dir(self.root.join("themes")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !names.contains(&self.selected) {
                    names.push(self.selected.clone());
                }
                return Ok(names);
            }
            Err(error) => return Err(error).context("list user themes"),
        };
        let mut custom = BTreeSet::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .context("theme filename is not UTF-8")?;
            validate_name(name)?;
            ensure!(
                !names.iter().any(|builtin| builtin == name),
                "user theme collides with a builtin name"
            );
            custom.insert(name.to_owned());
            ensure!(
                custom.len() <= 256,
                "user theme catalogue exceeds 256 files"
            );
        }
        names.extend(custom);
        if !names.contains(&self.selected) {
            names.push(self.selected.clone());
        }
        Ok(names)
    }
}
