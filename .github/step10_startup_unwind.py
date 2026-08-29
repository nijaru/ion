from pathlib import Path

main = Path("crates/ion/src/main.rs")
text = main.read_text()
helper_anchor = """async fn run_tui(cli: &Cli, settings: &Settings) -> ExitCode {
"""
helper = """fn restore_tui_startup_terminal(mut terminal: ion_terminal::TerminalSession) {
    let restore_error = terminal.restore().err();
    drop(terminal);
    if let Some(err) = restore_error {
        let _ = writeln!(io::stderr(), "terminal restore failed: {err}");
    }
}

async fn run_tui(cli: &Cli, settings: &Settings) -> ExitCode {
"""
assert text.count(helper_anchor) == 1, "run_tui anchor changed"
text = text.replace(helper_anchor, helper, 1)
replacements = [
(
"""        Err(err) => {
            let _ = writeln!(io::stderr(), "store: {err}");
            return ExitCode::FAILURE;
        }
    };
    // The startup notice is rendered inside the transcript (HostConfig):
""",
"""        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "store: {err}");
            return ExitCode::FAILURE;
        }
    };
    // The startup notice is rendered inside the transcript (HostConfig):
"""
),
(
"""            Ok(None) => {
                let _ = writeln!(io::stderr(), "no persisted session to resume");
                return ExitCode::from(2);
            }
            Err(err) => {
                let _ = writeln!(io::stderr(), "store: {err}");
                return ExitCode::FAILURE;
            }
""",
"""            Ok(None) => {
                restore_tui_startup_terminal(guard);
                let _ = writeln!(io::stderr(), "no persisted session to resume");
                if let Err(close_err) = store.close().await {
                    tracing::error!(error = %close_err, "failed to close the session store");
                }
                return ExitCode::from(2);
            }
            Err(err) => {
                restore_tui_startup_terminal(guard);
                let _ = writeln!(io::stderr(), "store: {err}");
                if let Err(close_err) = store.close().await {
                    tracing::error!(error = %close_err, "failed to close the session store");
                }
                return ExitCode::FAILURE;
            }
"""
),
(
"""        Err(err) => {
            let _ = writeln!(io::stderr(), "cwd: {err}");
            return ExitCode::FAILURE;
        }
    };
    let tools = match build_catalog(settings, cli).await {
""",
"""        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "cwd: {err}");
            if let Err(close_err) = store.close().await {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            return ExitCode::FAILURE;
        }
    };
    let tools = match build_catalog(settings, cli).await {
"""
),
(
"""        Err(err) => {
            let _ = writeln!(io::stderr(), "cwd: {err}");
            return ExitCode::FAILURE;
        }
    };
    let trusted_resources = match ion_core::load_trusted_resources(&cwd, cli.trust_project) {
""",
"""        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "cwd: {err}");
            if let Err(close_err) = store.close().await {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            return ExitCode::FAILURE;
        }
    };
    let trusted_resources = match ion_core::load_trusted_resources(&cwd, cli.trust_project) {
"""
),
(
"""        Err(err) => {
            let _ = writeln!(io::stderr(), "trusted resources: {err}");
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            return ExitCode::FAILURE;
        }
""",
"""        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "trusted resources: {err}");
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            if let Err(close_err) = store.close().await {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            return ExitCode::FAILURE;
        }
"""
),
(
"""            Err(err) => {
                let _ = writeln!(io::stderr(), "resume: {err}");
                if let Err(close_err) = tools.close().await {
                    tracing::error!(error = %close_err, "failed to close the tool catalog");
                }
                return ExitCode::FAILURE;
            }
""",
"""            Err(err) => {
                restore_tui_startup_terminal(guard);
                let _ = writeln!(io::stderr(), "resume: {err}");
                if let Err(close_err) = tools.close().await {
                    tracing::error!(error = %close_err, "failed to close the tool catalog");
                }
                if let Err(close_err) = store.close().await {
                    tracing::error!(error = %close_err, "failed to close the session store");
                }
                return ExitCode::FAILURE;
            }
"""
),
(
"""        Err(err) => {
            let _ = writeln!(io::stderr(), "agents: {err}");
            let _ = runtime.session().close().await;
            let _ = runtime.join().await;
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            let _ = store.close().await;
            return ExitCode::FAILURE;
        }
""",
"""        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "agents: {err}");
            if let Err(close_err) = runtime.session().close().await {
                tracing::error!(error = %close_err, "failed to close session after agent-host startup failure");
            }
            if let Err(join_err) = runtime.join().await {
                tracing::error!(error = %join_err, "runtime join failed after agent-host startup failure");
            }
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            if let Err(close_err) = store.close().await {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            return ExitCode::FAILURE;
        }
"""
),
]
for old, new in replacements:
    assert text.count(old) == 1, f"startup error path changed: {old[:80]!r}"
    text = text.replace(old, new, 1)
main.write_text(text)

pty = Path("crates/ion/tests/pty_restoration.rs")
text = pty.read_text()
old = """fn spawn_ion(envs: &[(&str, &str)]) -> PtySession {
    // A zero winsize would give the TUI an empty area to draw in.
"""
new = """fn spawn_ion(envs: &[(&str, &str)]) -> PtySession {
    spawn_ion_with_args(&[], envs)
}

fn spawn_ion_with_args(args: &[&str], envs: &[(&str, &str)]) -> PtySession {
    // A zero winsize would give the TUI an empty area to draw in.
"""
assert text.count(old) == 1, "PTY spawn helper changed"
text = text.replace(old, new, 1)
old = """    let mut command = Command::new(env!("CARGO_BIN_EXE_ion"));
    command.env("ION_SETTINGS", settings.path());
"""
new = """    let mut command = Command::new(env!("CARGO_BIN_EXE_ion"));
    command.args(args);
    command.env("ION_SETTINGS", settings.path());
"""
assert text.count(old) == 1, "PTY command construction changed"
text = text.replace(old, new, 1)
marker = """#[test]
fn clean_exit_restores_the_terminal() {
"""
assert text.count(marker) == 1, "clean-exit test marker changed"
regression = r'''#[test]
fn startup_error_restores_terminal_before_diagnostic() {
    let mut session = spawn_ion_with_args(&["--resume"], &[]);
    let status = session
        .wait_exit(std::time::Duration::from_secs(15))
        .expect("resume-without-session must exit");
    assert_eq!(status.code(), Some(2));
    assert!(
        session.wait_for_output(
            "no persisted session to resume",
            std::time::Duration::from_secs(2)
        ),
        "expected resume diagnostic"
    );

    let output = session.output.lock().expect("output lock");
    let text = String::from_utf8_lossy(&output);
    let restored = text
        .find("\x1b[?2004l")
        .expect("bracketed paste disable before startup diagnostic");
    let diagnostic = text
        .find("no persisted session to resume")
        .expect("resume diagnostic");
    assert!(
        restored < diagnostic,
        "startup diagnostic was emitted before terminal restoration: {text:?}"
    );
    drop(output);
    session.assert_cooked();
}

'''
text = text.replace(marker, regression + marker, 1)
pty.write_text(text)

design = Path("DESIGN.md")
text = design.read_text()
old = "10. Finish the interactive frontend against the stable session/agent-host contract, with ACP as a first-class sibling client. Validate pure UI configuration before terminal/runtime/session acquisition, keep `SessionHandle` as the only runtime mutation path, and preserve the established `TERMINAL.md` reducer/`TerminalSession` architecture rather than introducing another UI framework."
new = "10. Finish the interactive frontend against the stable session/agent-host contract, with ACP as a first-class sibling client. Validate pure UI configuration before terminal/runtime/session acquisition; after terminal acquisition, explicitly restore the terminal before startup diagnostics and unwind acquired store/catalog/runtime ownership on failure. Keep `SessionHandle` as the only runtime mutation path and preserve the established `TERMINAL.md` reducer/`TerminalSession` architecture rather than introducing another UI framework."
assert text.count(old) == 1, "Step 10 design text changed"
design.write_text(text.replace(old, new, 1))
