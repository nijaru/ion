use std::io::Write as _;
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
