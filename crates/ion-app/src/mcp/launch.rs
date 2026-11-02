//! Local server launch configuration, captured before any process is started.
use super::{config::McpStdioServer, values};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};
use tokio::process::Command;

pub(super) async fn command(
    server: &McpStdioServer,
    cwd: &Path,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Command> {
    let inherited: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    let mut command = Command::new(&server.command);
    command
        .args(&server.args)
        .current_dir(working_directory(server.cwd.as_deref(), cwd, &inherited)?)
        .env_clear()
        .envs(&inherited);
    let values = values::resolve(&server.env, &inherited, cwd, stop, |value| {
        anyhow::ensure!(
            !value.contains('\0'),
            "invalid MCP environment variable value"
        );
        Ok(())
    })
    .await?;
    command.envs(values);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
