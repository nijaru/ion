//! Operator-owned program veto, captured with the coding binding.
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use anyhow::{Result, ensure};
use ion_ai::BoxFuture;
use ion_core::{ToolDecision, ToolPolicy, ToolPolicyRequest};
use rustix::process::Pid;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const MAX_REPLY_BYTES: usize = 64 * 1024;
const MAX_REASON_BYTES: usize = 4096;
fn default_timeout() -> u64 {
    30_000
}

/// Explicit operator program/argv, not model input or project configuration.
/// The program runs through the binding's captured command environment. Its
/// timeout includes pipe exchange and exit, not a hard cleanup deadline.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolVetoSettings {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}
impl ToolVetoSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.program.is_empty() && !self.program.contains('\0'),
            "tool veto program must be nonempty and contain no NUL"
        );
        ensure!(
            !self.args.iter().any(|arg| arg.contains('\0')),
            "tool veto arguments cannot contain NUL"
        );
        ensure!(
            self.timeout_ms != 0,
            "tool veto timeout_ms must be positive"
        );
        ensure!(
            tokio::time::Instant::now()
                .checked_add(Duration::from_millis(self.timeout_ms))
                .is_some(),
            "tool veto timeout_ms exceeds the clock range"
        );
        Ok(())
    }
}

/// Retained program policy for an explicitly configured application. No program
/// runs at construction; supplied environment and workspace are captured once.
pub struct ToolVeto {
    settings: Arc<ToolVetoSettings>,
    environment: Arc<crate::CommandEnvironment>,
    cwd: PathBuf,
}
impl ToolVeto {
    pub fn new(
        cwd: &std::path::Path,
        settings: Arc<ToolVetoSettings>,
        environment: Arc<crate::CommandEnvironment>,
    ) -> Result<Self> {
        settings.validate()?;
        let cwd = std::fs::canonicalize(cwd)?;
        ensure!(cwd.is_dir(), "tool veto workspace must be a directory");
        Ok(Self {
            settings,
            environment,
            cwd,
        })
    }
}

#[derive(Serialize)]
struct Request<'a> {
    tool_name: &'a str,
    arguments: &'a serde_json::Value,
    call_id: &'a str,
    origin: ion_core::ToolOrigin,
    cwd: &'a std::path::Path,
}
#[derive(Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    // An empty struct is intentional: an internally tagged unit variant can
    // discard surplus fields even with deny_unknown_fields on the enum.
    Allow {},
    Deny { reason: String },
}

impl ToolPolicy for ToolVeto {
    fn decide<'a>(
        &'a self,
        request: ToolPolicyRequest<'a>,
        stop: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolDecision, String>> {
        Box::pin(async move {
            let mut input = serde_json::to_vec(&Request {
                tool_name: &request.call.name,
                arguments: &request.call.arguments,
                call_id: &request.call.id,
                origin: request.origin,
                cwd: &self.cwd,
            })
            .map_err(|_| "cannot encode tool veto input")?;
            input.push(b'\n');
            let mut command = crate::command_args::quote(&self.settings.program);
            for arg in &self.settings.args {
                command.push(' ');
                command.push_str(&crate::command_args::quote(arg));
            }
            let mut command = self.environment.command(&command);
            command.current_dir(&self.cwd);
            let deadline = tokio::time::Instant::now()
                .checked_add(Duration::from_millis(self.settings.timeout_ms))
                .ok_or("tool veto timeout_ms exceeds the clock range")?;
            // The retained custodian owns the program even if this borrowed
            // callback is dropped. Stop requests never discard a live child.
            let mut preparation = crate::preparation::Preparation::start(
                stop.child_token(),
                move |stop| async move {
                    run(command, input, deadline, stop)
                        .await
                        .map_err(anyhow::Error::msg)
                },
            );
            preparation.join().await.map_err(|error| error.to_string())
        })
    }
}

async fn run(
    mut command: tokio::process::Command,
    input: Vec<u8>,
    deadline: tokio::time::Instant,
    stop: CancellationToken,
) -> Result<ToolDecision, String> {
    if stop.is_cancelled() || deadline <= tokio::time::Instant::now() {
        return Err("tool veto stopped before program start".into());
    }
    // Subscribe before spawn so even a very short-lived child cannot race the
    // first non-reaping exit check. Tokio's process listener is independent.
    let mut exits = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())
        .map_err(|_| "cannot observe tool veto program exit")?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| "cannot start tool veto program")?;
    let pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(Pid::from_raw)
        .expect("a spawned unreaped native child has a positive Unix pid");
    let mut stdin = child.stdin.take().expect("piped policy stdin");
    let mut stdout = child.stdout.take().expect("piped policy stdout");
    let mut stderr = child.stderr.take().expect("piped policy stderr");
    let result = {
        let exchange = async {
            let write = async move {
                stdin
                    .write_all(&input)
                    .await
                    .map_err(|_| "cannot write tool veto input")?;
                let closed = stdin
                    .shutdown()
                    .await
                    .map_err(|_| "cannot close tool veto input");
                // ChildStdin's shutdown does not close the owned pipe on every
                // platform. The request ends at EOF, before waiting for a reply.
                drop(stdin);
                closed
            };
            let read = async {
                let mut bytes = Vec::new();
                (&mut stdout)
                    .take((MAX_REPLY_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| "cannot read tool veto reply")?;
                if bytes.len() > MAX_REPLY_BYTES {
                    return Err("tool veto reply exceeds 65536 bytes");
                }
                Ok(bytes)
            };
            // Diagnostics are drained, not retained, spooled or disclosed to
            // the model: they may contain private policy code or credentials.
            let discard = async {
                tokio::io::copy(&mut stderr, &mut tokio::io::sink())
                    .await
                    .map(|_| ())
                    .map_err(|_| "cannot drain tool veto diagnostics")
            };
            let ((), bytes, ()) = tokio::try_join!(write, read, discard)?;
            if !crate::local_tools::command::observe_exit(pid, &mut exits)
                .await
                .map_err(|_| "cannot observe tool veto program exit")?
            {
                return Err("tool veto program exited unsuccessfully");
            }
            let reply: Reply =
                serde_json::from_slice(&bytes).map_err(|_| "invalid tool veto decision JSON")?;
            match reply {
                Reply::Allow {} => Ok(ToolDecision::Allow),
                Reply::Deny { reason }
                    if !reason.trim().is_empty() && reason.len() <= MAX_REASON_BYTES =>
                {
                    Ok(ToolDecision::Deny { reason })
                }
                Reply::Deny { .. } => {
                    Err("tool veto denial reason must be nonblank and at most 4096 bytes")
                }
            }
        };
        tokio::pin!(exchange);
        tokio::select! {
            biased;
            () = stop.cancelled() => Err("tool veto cancelled"),
            () = tokio::time::sleep_until(deadline) => Err("tool veto timed out"),
            result = &mut exchange => result,
        }
    };
    let result = if result.is_ok() && deadline <= tokio::time::Instant::now() {
        Err("tool veto decision exceeded its deadline")
    } else {
        result
    };
    // Every terminal path ends this one-shot program scope. Even a failed
    // program can leave same-group descendants with closed pipes. WNOWAIT keeps
    // its PID reserved until this signal, avoiding an unrelated reused group.
    crate::local_tools::command::kill_child(&mut child, Some(pid))
        .await
        .map_err(|_| "tool veto program joining failed")?;
    if stop.is_cancelled() {
        return Err("tool veto cancelled before decision publication".into());
    }
    result.map_err(str::to_owned)
}
