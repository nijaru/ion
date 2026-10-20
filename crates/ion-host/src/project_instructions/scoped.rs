//! Live instruction observations for explicit native filesystem targets.
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{load_sources, shadowed_main_agents};

#[derive(Clone, Copy)]
pub(crate) enum TargetKind {
    File,
    Directory,
    SearchRoot,
}

pub(crate) struct ScopedInstructions {
    pub target: PathBuf,
    pub observation: Value,
    has_rules: bool,
}

impl ScopedInstructions {
    /// Consult only tool results actually included in the issuing request, not
    /// an executor-local "seen" cache. Failed commits, withholding, compaction,
    /// reopen and composed-call output selection therefore need no second owner.
    pub fn requires_review<'a>(&self, results: impl DoubleEndedIterator<Item = &'a Value>) -> bool {
        for result in results.rev() {
            if let Some(matches) = find_observation(result, &self.observation, 0) {
                return !matches;
            }
        }
        self.has_rules
    }
}

pub(crate) fn load_scoped(
    cwd: &Path,
    target: &Path,
    kind: TargetKind,
    stop: &CancellationToken,
) -> Result<ScopedInstructions> {
    let target = resolve_target(target)
        .with_context(|| format!("cannot resolve instruction target {}", target.display()))?;
    let directory = match kind {
        TargetKind::Directory => target.as_path(),
        TargetKind::SearchRoot if target.is_dir() => target.as_path(),
        _ => target
            .parent()
            .context("instruction target has no parent")?,
    };
    let shadowed = shadowed_main_agents(directory);
    let standing_shadowed = shadowed_main_agents(cwd);
    let mut sources = Vec::new();
    for ancestor in directory.ancestors() {
        ensure!(
            !stop.is_cancelled(),
            "cancelled before instruction discovery"
        );
        // These sources belong to the captured standing resources; changes to
        // them take effect at resource reload, not midway through admitted work.
        let path = ancestor.join("AGENTS.md");
        if cwd.starts_with(ancestor) && standing_shadowed.as_ref() != Some(&path) {
            continue;
        }
        if shadowed.as_ref() != Some(&path) {
            sources.push((path, "Project"));
        }
    }
    let mut prefix = String::new();
    if let Some(main) = shadowed {
        prefix = format!(
            "\nFor this target's linked worktree, the main-checkout project source {} is shadowed by the worktree's AGENTS.md, not inherited alongside it.\n",
            serde_json::to_string(&main)?
        );
    }
    let instructions = load_sources(prefix, sources)?;
    ensure!(
        !stop.is_cancelled(),
        "cancelled during instruction discovery"
    );
    let has_rules = !instructions.text.is_empty();
    let directory = serde_json::to_value(directory)
        .context("instruction target directory is not representable as UTF-8")?;
    Ok(ScopedInstructions {
        target,
        observation: json!({"directory": directory, "text": instructions.text}),
        has_rules,
    })
}

/// Resolve actual symlinks, including existing prefixes of a new write target,
/// without creating directories. Lexical normalization before resolving links
/// would select the wrong rules for `link/../file`.
fn resolve_target(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => return Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            _ => {
                resolved.push(component.as_os_str());
                match resolved.canonicalize() {
                    Ok(path) => resolved = path,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        // An existing dangling symlink is not a missing directory
                        // we can hypothetically create for a new-file write.
                        if fs::symlink_metadata(&resolved).is_ok() {
                            return Err(error);
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(resolved)
}

/// A structured return or JSON text selected by Code Mode can carry the same
/// observation. Hidden child results cannot. If one selected result contains
/// conflicting snapshots of this directory, require a fresh unambiguous one.
fn find_observation(value: &Value, expected: &Value, depth: usize) -> Option<bool> {
    if depth >= 128 {
        return None;
    }
    let direct =
        if value.get("directory") == expected.get("directory") && value.get("text").is_some() {
            Some(value == expected)
        } else {
            None
        };
    let enclosed = value
        .get("project_instructions")
        .filter(|observation| observation.get("directory") == expected.get("directory"))
        .map(|observation| observation == expected);
    let merge = |a: Option<bool>, b: Option<bool>| match (a, b) {
        (Some(a), Some(b)) => Some(a && b),
        (a, b) => a.or(b),
    };
    let found = merge(direct, enclosed);
    match value {
        Value::Array(values) => values.iter().fold(found, |found, value| {
            merge(found, find_observation(value, expected, depth + 1))
        }),
        Value::Object(values) => values.values().fold(found, |found, value| {
            merge(found, find_observation(value, expected, depth + 1))
        }),
        Value::String(text)
            if text.trim_start().starts_with('{') || text.trim_start().starts_with('[') =>
        {
            serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|value| find_observation(&value, expected, depth + 1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn targets_follow_physical_paths_without_creating_missing_parents() {
        let root = std::env::temp_dir().join(format!("ion-rule-targets-{}", uuid::Uuid::now_v7()));
        let cwd = root.join("workspace");
        let external = root.join("external");
        fs::create_dir_all(cwd.join("sub")).unwrap();
        fs::create_dir_all(external.join("child")).unwrap();
        fs::write(cwd.join("AGENTS.md"), "STANDING_RULE").unwrap();
        fs::write(cwd.join("sub/AGENTS.md"), "LOCAL_RULE").unwrap();
        fs::write(external.join("AGENTS.md"), "EXTERNAL_RULE").unwrap();
        symlink(&external, cwd.join("link")).unwrap();
        symlink(external.join("child"), cwd.join("child-link")).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let stop = CancellationToken::new();
        for (target, marker) in [
            (cwd.join("sub/new/deeper/file"), "LOCAL_RULE"),
            (cwd.join("link/new/file"), "EXTERNAL_RULE"),
            (cwd.join("child-link/../new/file"), "EXTERNAL_RULE"),
        ] {
            let scope = load_scoped(&cwd, &target, TargetKind::File, &stop).unwrap();
            assert!(scope.observation["text"].as_str().unwrap().contains(marker));
            assert!(
                !scope.observation["text"]
                    .as_str()
                    .unwrap()
                    .contains("STANDING_RULE")
            );
            assert!(scope.requires_review(std::iter::empty()));
        }
        assert!(!cwd.join("sub/new").exists());
        assert!(!external.join("new").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn latest_selected_observation_wins_and_conflicts_require_review() {
        let current = json!({"directory":"/repo/sub", "text":"CURRENT_RULE"});
        let scope = ScopedInstructions {
            target: PathBuf::from("/repo/sub/file"),
            observation: current.clone(),
            has_rules: true,
        };
        let old = json!({"project_instructions":{"directory":"/repo/sub", "text":"OLD_RULE"}});
        let selected = json!({"project_instructions":current});
        assert!(!scope.requires_review([&old, &selected].into_iter()));
        assert!(!scope.requires_review([&current].into_iter()));
        assert!(!scope.requires_review(
            [&json!({"output": [{"type":"text", "text":current.to_string()}]})].into_iter()
        ));
        assert!(scope.requires_review([&selected, &old].into_iter()));
        let wrapped = json!({"result":{"value":selected}});
        let logged =
            json!({"output":[{"type":"text", "text":serde_json::to_string(&wrapped).unwrap()}]});
        assert!(!scope.requires_review([&logged].into_iter()));
        let conflicting = json!({"result":[selected,old]});
        assert!(scope.requires_review([&conflicting].into_iter()));
        let conflicting_siblings = json!({"project_instructions":current, "previous":old});
        assert!(scope.requires_review([&conflicting_siblings].into_iter()));
        let removed = ScopedInstructions {
            target: PathBuf::from("/repo/sub/file"),
            observation: json!({"directory":"/repo/sub", "text":""}),
            has_rules: false,
        };
        assert!(!removed.requires_review(std::iter::empty()));
        assert!(removed.requires_review([&selected].into_iter()));
    }
}
