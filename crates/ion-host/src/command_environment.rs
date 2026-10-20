//! Trusted native-command configuration, captured by a Host binding, never model arguments.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::process::Command;

const MAX_SETTINGS_BYTES: usize = 64 * 1024;
const SETUP_VARIABLE: &str = "__ION_SHELL_SETUP";
// Keep setup out of argv/history. Evaluate the requested command only after
// successful setup, in the same shell so exported values/functions are available.
// Disable inherited tracing before touching private code (BASH_XTRACEFD may
// target stdout). Suppress evaluator stderr; restore tracing for the payload.
const SETUP_BOOTSTRAP: &str = r#"case $- in
    *x*) set +x; __ion_trace=1 ;;
    *) __ion_trace=0 ;;
esac
__ion_command=$1
set --
{
    __ion_setup=$__ION_SHELL_SETUP
    unset __ION_SHELL_SETUP
    eval "$__ion_setup"
    __ion_status=$?
    unset __ion_setup
} 2>/dev/null
if [ "$__ion_status" -ne 0 ]; then
    printf 'Ion shell setup failed; requested command was not run.\n' >&2
    exit "$__ion_status"
fi
if [ "$__ion_trace" -eq 1 ]; then set -x; fi
eval "$__ion_command"
"#;

/// An immutable local command environment. This is not a sandbox or a durable
/// grant. Hosts reconstruct it and attest their existing AuthorityScope before
/// continuation; neither a shell path nor equal tool declarations prove authority.
/// Values/setup are deliberately not serializable or Debug output.
pub struct CommandEnvironment {
    program: PathBuf,
    variables: BTreeMap<OsString, OsString>,
    setup: Option<String>,
}

impl CommandEnvironment {
    /// Use an explicit Bash-compatible executable and complete environment map.
    /// No process environment, user configuration or startup is loaded here.
    pub fn new(program: PathBuf, variables: BTreeMap<OsString, OsString>) -> io::Result<Self> {
        if !program.is_absolute() {
            return Err(invalid("command shell must be an absolute path"));
        }
        let metadata = fs::metadata(&program)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(invalid("command shell is not an executable file"));
        }
        for (name, value) in &variables {
            let name = name.as_bytes();
            if name.is_empty()
                || name.contains(&b'=')
                || name.contains(&0)
                || value.as_bytes().contains(&0)
            {
                return Err(invalid("invalid command environment entry"));
            }
        }
        Ok(Self {
            program,
            variables,
            setup: None,
        })
    }

    /// Capture the launching process once. Bash is preferred, with the existing
    /// POSIX sh fallback. Normal shell semantics (including BASH_ENV) still apply;
    /// this does not execute an additional login/interactive startup capture.
    /// A missing default shell is reported at spawn, so filesystem-only users
    /// of LocalTools do not require an installed interpreter.
    pub fn inherited() -> Self {
        let variables = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let candidates = std::iter::once(PathBuf::from("/bin/bash"))
            .chain(
                variables
                    .get(std::ffi::OsStr::new("PATH"))
                    .into_iter()
                    .flat_map(std::env::split_paths)
                    .map(|directory| directory.join("bash")),
            )
            .chain(std::iter::once(PathBuf::from("/bin/sh")));
        let program = candidates
            .filter_map(|path| std::path::absolute(path).ok())
            .find(|path| {
                fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Self {
            program,
            variables,
            setup: None,
        }
    }

    /// Explicit trusted shell code, run before each requested command. It can
    /// perform effects and access this environment; it is not an inert env file.
    pub fn with_setup(mut self, setup: String) -> io::Result<Self> {
        if setup.contains('\0') || setup.len() > MAX_SETTINGS_BYTES {
            return Err(invalid("shell setup contains NUL or exceeds 65536 bytes"));
        }
        self.setup = (!setup.is_empty()).then_some(setup);
        Ok(self)
    }

    pub(crate) fn coding(config_root: &Path) -> Result<Self> {
        let path = config_root.join("settings.json");
        let settings: Settings = match crate::file_io::read_bounded(&path, MAX_SETTINGS_BYTES) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                // Serde's ordinary Display can include an invalid string value,
                // which may be setup code or a credential. Report location only.
                anyhow::anyhow!(
                    "invalid shell settings in {} at line {}, column {}",
                    path.display(),
                    error.line(),
                    error.column()
                )
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        let mut environment = Self::inherited();
        if let Some(program) = settings.shell.path {
            let program = if program.starts_with("~/") {
                PathBuf::from(
                    environment
                        .variables
                        .get(std::ffi::OsStr::new("HOME"))
                        .context("HOME is required to expand shell.path")?,
                )
                .join(program.strip_prefix("~/").expect("checked prefix"))
            } else {
                program
            };
            environment = Self::new(program, environment.variables)
                .context("invalid configured Bash-compatible shell")?;
        }
        environment
            .with_setup(settings.shell.setup.unwrap_or_default())
            .map_err(Into::into)
    }

    pub(crate) fn command(&self, text: &str) -> Command {
        let mut command = Command::new(&self.program);
        command.env_clear().envs(&self.variables);
        match &self.setup {
            Some(setup) => {
                command
                    .env(SETUP_VARIABLE, setup)
                    .args(["-c", SETUP_BOOTSTRAP])
                    .arg(&self.program)
                    .arg(text);
            }
            None => {
                command.arg("-c").arg(text);
            }
        }
        command
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    shell: ShellSettings,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ShellSettings {
    path: Option<PathBuf>,
    setup: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalTools, UserShell};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn environment() -> CommandEnvironment {
        CommandEnvironment::new(
            PathBuf::from("/bin/bash"),
            [
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("VIRTUAL_ENV".into(), "/chosen/venv".into()),
            ]
            .into(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn explicit_environment_and_prepared_user_shell_retain_setup_and_literal_command() {
        let environment = Arc::new(environment().with_setup(
            "export SETUP_VALUE=bound; shopt -s expand_aliases; alias probe='printf alias'; helper() { printf '%s' \"$VIRTUAL_ENV:$SETUP_VALUE\"; }".into()
        ).unwrap());
        let tools =
            LocalTools::with_environment(std::env::temp_dir(), environment.clone()).unwrap();
        let prepared = tools
            .prepare("helper; printf '|'; probe; printf '|%s' '$(false)'")
            .unwrap();
        drop(tools);
        let output = prepared(CancellationToken::new()).await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["stdout"], "/chosen/venv:bound|alias|$(false)");
        let native = Arc::new(
            LocalTools::with_environment(std::env::temp_dir(), environment.clone()).unwrap(),
        );
        let catalog =
            ion_core::ToolSet::new([native as Arc<dyn ion_core::CodingToolSource>]).snapshot();
        let observed = catalog
            .execute_untracked(
                &ion_ai::ToolCall {
                    id: "env".into(),
                    name: "exec".into(),
                    arguments: serde_json::json!({"command":"helper"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(!observed.is_error, "{}", observed.value);
        assert_eq!(observed.value["stdout"], "/chosen/venv:bound");
        let command = environment.command("helper");
        assert!(!command.as_std().get_args().any(|arg| {
            arg.as_bytes()
                .windows(12)
                .any(|bytes| bytes == b"SETUP_VALUE=")
        }));
    }

    #[tokio::test]
    async fn setup_evaluator_errors_do_not_publish_private_code() {
        // Ubuntu's /bin/bash is modern; on macOS also exercise Homebrew Bash
        // when installed, since the system Bash predates BASH_XTRACEFD.
        for program in ["/bin/bash", "/opt/homebrew/bin/bash"] {
            if !Path::new(program).is_file() {
                continue;
            }
            for fd in [None, Some("1"), Some("2")] {
                for malformed in [false, true] {
                    let mut environment = environment();
                    environment.program = program.into();
                    if let Some(fd) = fd {
                        environment
                            .variables
                            .insert("SHELLOPTS".into(), "xtrace".into());
                        environment
                            .variables
                            .insert("BASH_XTRACEFD".into(), fd.into());
                    }
                    let setup = format!(
                        "export TOKEN=SYNTHETIC_SECRET{}",
                        if malformed { " )" } else { "" }
                    );
                    let tools = LocalTools::with_environment(
                        std::env::temp_dir(),
                        Arc::new(environment.with_setup(setup).unwrap()),
                    )
                    .unwrap();
                    let output = tools
                        .run_user_shell("printf PAYLOAD", CancellationToken::new())
                        .await;
                    assert_eq!(output.is_error, malformed, "{}", output.value);
                    let recorded = serde_json::to_string(&output.value).unwrap();
                    assert!(!recorded.contains("SYNTHETIC_SECRET"), "{recorded}");
                    assert_eq!(
                        output.value["stdout"].as_str().unwrap().contains("PAYLOAD"),
                        !malformed
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn setup_transport_preserves_payload_positional_parameters() {
        let text = "printf '%s|%s|%s|%s' \"$#\" \"${1-unset}\" \"$0\" \"$*\"";
        let plain = environment().command(text).output().await.unwrap();
        let configured = environment()
            .with_setup(":".into())
            .unwrap()
            .command(text)
            .output()
            .await
            .unwrap();
        assert!(plain.status.success() && configured.status.success());
        assert_eq!(configured.stdout, plain.stdout);
    }

    #[tokio::test]
    async fn failed_setup_never_runs_requested_command() {
        let environment = Arc::new(
            environment()
                .with_setup("printf SETUP; false".into())
                .unwrap(),
        );
        let tools = LocalTools::with_environment(std::env::temp_dir(), environment).unwrap();
        let output = tools
            .run_user_shell("printf FORBIDDEN", CancellationToken::new())
            .await;
        assert!(output.is_error);
        assert_eq!(output.value["exit_code"], 1);
        assert_eq!(output.value["stdout"], "SETUP");
        assert!(
            output.value["stderr"]
                .as_str()
                .unwrap()
                .contains("requested command was not run")
        );
    }

    #[tokio::test]
    async fn cancellation_during_setup_is_joined_without_starting_payload() {
        let root = std::env::temp_dir().join(format!("ion-setup-stop-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let environment = Arc::new(
            environment()
                .with_setup("printf ready > ready; exec sleep 30".into())
                .unwrap(),
        );
        let tools = LocalTools::with_environment(&root, environment).unwrap();
        let stop = CancellationToken::new();
        let work = tools.run_user_shell("touch forbidden", stop.clone());
        let cancel = async {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !root.join("ready").exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            stop.cancel();
        };
        let (output, ()) = tokio::join!(work, cancel);
        assert!(output.is_error);
        assert_eq!(output.value["cancelled"], true);
        assert!(!root.join("forbidden").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_is_explicit_bounded_and_errors_do_not_echo_values() {
        let root =
            std::env::temp_dir().join(format!("ion-shell-settings-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("settings.json");
        fs::write(&path, r#"{"shell":{"setup":"export MARKER=first"}}"#).unwrap();
        let first = CommandEnvironment::coding(&root).unwrap();
        fs::write(&path, r#"{"shell":{"setup":"export MARKER=second"}}"#).unwrap();
        assert_eq!(first.setup.as_deref(), Some("export MARKER=first"));
        assert_eq!(
            CommandEnvironment::coding(&root).unwrap().setup.as_deref(),
            Some("export MARKER=second")
        );
        for invalid in [
            r#"{"shell":"PRIVATE_VALUE"}"#,
            r#"{"shell":{"login":true}}"#,
        ] {
            fs::write(&path, invalid).unwrap();
            let error = CommandEnvironment::coding(&root).err().unwrap().to_string();
            assert!(!error.contains("PRIVATE_VALUE"));
            assert!(error.contains("line"));
        }
        fs::write(&path, vec![b' '; MAX_SETTINGS_BYTES + 1]).unwrap();
        assert!(CommandEnvironment::coding(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
