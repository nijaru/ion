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
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", cwd.display()))?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let paths = paths
            .into_iter()
            .map(|path| {
                ensure!(
                    !path.as_os_str().is_empty(),
                    "resource path must not be empty"
                );
                let path = if let Ok(suffix) = path.strip_prefix("~") {
                    home.as_ref()
                        .context("HOME is not set for resource path")?
                        .join(suffix)
                } else {
                    path
                };
                Ok(if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            include_defaults,
            paths,
        })
    }
}
