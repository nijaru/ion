//! Invocation-local resource sources, anchored before workspace changes.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

/// Independently select skill and prompt-template sources for a Host recipe.
#[derive(Debug, Clone, Default)]
pub struct ResourceSelection {
    pub skills: ResourcePaths,
    pub prompt_templates: ResourcePaths,
}

/// Default discovery plus ordered explicit files/directories for one resource kind.
/// Paths are captured once; default discovery still follows the binding cwd.
#[derive(Debug, Clone)]
pub struct ResourcePaths {
    pub(super) include_defaults: bool,
    pub(super) paths: Vec<PathBuf>,
}

impl Default for ResourcePaths {
    fn default() -> Self {
        Self {
            include_defaults: true,
            paths: Vec::new(),
        }
    }
}

impl ResourcePaths {
    /// Anchor paths to the invocation cwd. Only a leading `~` component expands;
    /// no environment references or commands are evaluated.
    pub fn new(cwd: &Path, include_defaults: bool, paths: Vec<PathBuf>) -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Ok(Self {
            include_defaults,
            paths: resolve_paths(cwd, home.as_deref(), paths)?,
        })
    }

    pub(super) fn append_configured(
        &mut self,
        config_root: &Path,
        home: Option<&Path>,
        paths: Vec<PathBuf>,
    ) -> Result<()> {
        if !paths.is_empty() {
            self.paths.extend(resolve_paths(config_root, home, paths)?);
        }
        Ok(())
    }
}

fn resolve_paths(base: &Path, home: Option<&Path>, paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let base = base
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", base.display()))?;
    paths
        .into_iter()
        .map(|path| {
            ensure!(
                !path.as_os_str().is_empty(),
                "resource path must not be empty"
            );
            let path = crate::expand_home_path(&path, home)
                .context("cannot expand resource path")?
                .into_owned();
            Ok(if path.is_absolute() {
                path
            } else {
                base.join(path)
            })
        })
        .collect()
}
