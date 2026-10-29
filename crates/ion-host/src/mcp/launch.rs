//! Local server launch configuration, captured before any process is started.
use super::config::{McpStdioServer, valid_env_name};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};
use tokio::process::Command;

// Bound interpolation amplification, not the inherited native environment.
const MAX_OVERRIDE_BYTES: usize = 1024 * 1024;

pub(super) fn command(server: &McpStdioServer, cwd: &Path) -> Result<Command> {
    let inherited: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    let mut command = Command::new(&server.command);
    command
        .args(&server.args)
        .current_dir(working_directory(server.cwd.as_deref(), cwd, &inherited)?)
        .env_clear()
        .envs(&inherited);
    let mut remaining = MAX_OVERRIDE_BYTES;
    for (name, value) in &server.env {
        remaining = remaining
            .checked_sub(name.len())
            .context("MCP environment overrides exceed byte limit")?;
        let value = expand(value, &inherited, &mut remaining)
            .with_context(|| format!("cannot resolve MCP environment variable {name}"))?;
        command.env(name, value);
    }
    Ok(command)
}

fn working_directory(
    configured: Option<&Path>,
    cwd: &Path,
    inherited: &BTreeMap<OsString, OsString>,
) -> Result<PathBuf> {
    let Some(directory) = configured else {
        return Ok(cwd.to_owned());
    };
    if let Ok(relative) = directory.strip_prefix("~") {
        let home = inherited
            .get(OsStr::new("HOME"))
            .filter(|home| !home.is_empty())
            .context("MCP working directory needs HOME")?;
        return Ok(cwd.join(Path::new(home).join(relative)));
    }
    Ok(cwd.join(directory))
}

fn expand(
    mut input: &str,
    inherited: &BTreeMap<OsString, OsString>,
    remaining: &mut usize,
) -> Result<String> {
    let mut output = String::new();
    let mut append = |part: &str| -> Result<()> {
        *remaining = remaining
            .checked_sub(part.len())
            .context("MCP environment overrides exceed byte limit")?;
        output.push_str(part);
        Ok(())
    };
    while let Some((literal, expression)) = input.split_once("${") {
        append(literal)?;
        let (expression, rest) = expression
            .split_once('}')
            .context("unterminated MCP environment reference")?;
        let (name, fallback) = expression
            .split_once(":-")
            .map_or((expression, None), |(name, value)| (name, Some(value)));
        ensure!(valid_env_name(name), "invalid MCP environment reference");
        let value = match inherited.get(OsStr::new(name)) {
            Some(value) => value
                .to_str()
                .context("MCP environment reference is not UTF-8")?,
            None => fallback.with_context(|| format!("MCP environment needs {name}"))?,
        };
        append(value)?;
        input = rest;
    }
    append(input)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn directories_use_session_cwd_or_explicit_parent_home() {
        let inherited = BTreeMap::from([("HOME".into(), "/parent-home".into())]);
        for (configured, expected) in [
            (None, "relative-session"),
            (Some("server data"), "relative-session/server data"),
            (Some("/absolute"), "/absolute"),
            (Some("~/server data"), "/parent-home/server data"),
            (Some("~"), "/parent-home"),
            (Some("~someone"), "relative-session/~someone"),
        ] {
            assert_eq!(
                working_directory(
                    configured.map(Path::new),
                    Path::new("relative-session"),
                    &inherited
                )
                .unwrap(),
                Path::new(expected)
            );
        }
        assert!(
            working_directory(
                Some(Path::new("~/server")),
                Path::new("/session"),
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn interpolation_is_bounded_non_recursive_and_never_executes_commands() {
        let inherited = BTreeMap::from([
            ("KEY".into(), "secret${MISSING}".into()),
            ("EMPTY".into(), "".into()),
            ("BINARY".into(), OsString::from_vec(vec![0xff])),
        ]);
        for (input, expected) in [
            ("literal $KEY !command", "literal $KEY !command"),
            ("before-${KEY}-after", "before-secret${MISSING}-after"),
            ("${ABSENT:-fallback}", "fallback"),
            ("${EMPTY:-fallback}", ""),
            ("${ABSENT:-}", ""),
        ] {
            assert_eq!(expand(input, &inherited, &mut 128).unwrap(), expected);
        }
        for input in ["${MISSING}", "${KEY", "${}", "${1KEY}", "${BINARY}"] {
            assert!(expand(input, &inherited, &mut 128).is_err(), "{input}");
        }
        assert!(expand("${KEY}${KEY}", &inherited, &mut 20).is_err());
        assert!(expand("literal", &inherited, &mut 6).is_err());
        assert_eq!(expand("literal", &inherited, &mut 7).unwrap(), "literal");
    }
}
