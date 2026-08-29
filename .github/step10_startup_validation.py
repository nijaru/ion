from pathlib import Path

main = Path("crates/ion/src/main.rs")
text = main.read_text()
old_start = """async fn run_tui(cli: &Cli, settings: &Settings) -> ExitCode {
    // The runtime owns model selection: /model <id> commits a durable
"""
new_start = """async fn run_tui(cli: &Cli, settings: &Settings) -> ExitCode {
    // Validate pure UI configuration before acquiring terminal, store, runtime,
    // or agent-host ownership. A malformed binding must not create durable
    // session state merely because interactive startup was attempted.
    let keymap = match tui::KeyMap::from_settings(&settings.keybindings) {
        Ok(keymap) => keymap,
        Err(err) => {
            let _ = writeln!(io::stderr(), "settings: {err}");
            return ExitCode::from(2);
        }
    };

    // The runtime owns model selection: /model <id> commits a durable
"""
assert text.count(old_start) == 1, "run_tui start changed"
text = text.replace(old_start, new_start, 1)
old_late = """    let keymap = match tui::KeyMap::from_settings(&settings.keybindings) {
        Ok(keymap) => keymap,
        Err(err) => {
            let _ = writeln!(io::stderr(), "settings: {err}");
            if let Err(agent_err) = agent_host.close().await {
                tracing::error!(error = %agent_err, "failed to close agent host");
            }
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            return ExitCode::from(2);
        }
    };
"""
assert text.count(old_late) == 1, "late keymap validation changed"
main.write_text(text.replace(old_late, "", 1))

regression = Path("crates/ion/tests/startup_validation.rs")
assert not regression.exists(), "startup validation test already exists"
regression.write_text(r'''use std::io::Write as _;
use std::process::Command;

#[test]
fn invalid_keybinding_fails_before_terminal_or_session_acquisition() {
    let mut settings = tempfile::NamedTempFile::new().expect("temp settings");
    writeln!(
        settings,
        "theme = \"dark\"\n\n[keybindings]\nquit = \"not-a-key\""
    )
    .expect("write settings");
    let data_root = tempfile::tempdir().expect("temp data root");

    // No PTY is supplied deliberately. Keybinding validation is pure launch
    // configuration and must fail before terminal setup is attempted.
    let output = Command::new(env!("CARGO_BIN_EXE_ion"))
        .env("ION_SETTINGS", settings.path())
        .env("XDG_DATA_HOME", data_root.path())
        .output()
        .expect("run ion");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("settings: unknown key \"not-a-key\" in binding \"not-a-key\""),
        "unexpected stderr: {stderr}"
    );
    assert!(
        !stderr.contains("terminal"),
        "terminal setup ran before settings validation: {stderr}"
    );
    assert!(
        !data_root.path().join("ion").exists(),
        "invalid UI configuration must not create the durable data directory"
    );
}
''')

design = Path("DESIGN.md")
text = design.read_text()
old = "10. Redesign the TUI only after the authoritative session/agent host contract is coherent, with ACP as a first-class client boundary."
new = "10. Finish the interactive frontend against the stable session/agent-host contract, with ACP as a first-class sibling client. Validate pure UI configuration before terminal/runtime/session acquisition, keep `SessionHandle` as the only runtime mutation path, and preserve the established `TERMINAL.md` reducer/`TerminalSession` architecture rather than introducing another UI framework."
assert text.count(old) == 1, "Step 10 design text changed"
design.write_text(text.replace(old, new, 1))
