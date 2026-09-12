mod p1_support;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::Command;

use p1_support::{AdmissionMode, EffectSpec, PrototypeStore, Recovery, RecoveryDecision};
use tempfile::tempdir;

fn append_witness(path: &std::path::Path, text: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open witness");
    writeln!(file, "{text}").expect("write witness");
    file.sync_all().expect("sync witness");
}

fn setup_crash_fixture(recovery: Recovery) {
    let db = std::env::var_os("ION_P1_CRASH_DB").expect("crash db");
    let witness = std::env::var_os("ION_P1_CRASH_WITNESS").expect("crash witness");
    let mut store = PrototypeStore::open(&db).expect("store");
    let receipt = store
        .admit("crash", store.root(), "external", AdmissionMode::Prompt)
        .expect("admit");
    store.start_task(receipt.task_id).expect("start");
    let effects = store
        .open_effects(
            receipt.task_id,
            &[EffectSpec {
                ordinal: 0,
                name: "external",
                recovery,
            }],
        )
        .expect("open effect");
    assert_eq!(effects.len(), 1);
    append_witness(std::path::Path::new(&witness), "effect happened");
}

#[test]
fn crash_child_never_replay() {
    if std::env::var_os("ION_P1_CRASH_DB").is_none()
        || std::env::var("ION_P1_CRASH_KIND").ok().as_deref() != Some("never")
    {
        return;
    }
    setup_crash_fixture(Recovery::NeverReplay);
    std::process::exit(73);
}

#[test]
fn crash_child_replay_safe() {
    if std::env::var_os("ION_P1_CRASH_DB").is_none()
        || std::env::var("ION_P1_CRASH_KIND").ok().as_deref() != Some("replay")
    {
        return;
    }
    setup_crash_fixture(Recovery::ReplaySafe);
    std::process::exit(74);
}

#[test]
fn abrupt_reopen_retries_safe_effect_but_preserves_never_replay_uncertainty() {
    let directory = tempdir().expect("tempdir");
    let executable = std::env::current_exe().expect("test executable");

    let replay_db = directory.path().join("replay.sqlite");
    let replay_witness = directory.path().join("replay.witness");
    let replay_status = Command::new(&executable)
        .arg("--exact")
        .arg("crash_child_replay_safe")
        .arg("--nocapture")
        .env("ION_P1_CRASH_DB", &replay_db)
        .env("ION_P1_CRASH_WITNESS", &replay_witness)
        .env("ION_P1_CRASH_KIND", "replay")
        .status()
        .expect("run replay child");
    assert_eq!(replay_status.code(), Some(74));
    let mut replay_store = PrototypeStore::open(&replay_db).expect("reopen replay");
    assert!(matches!(
        replay_store
            .recover_pending()
            .expect("recover replay")
            .as_slice(),
        [RecoveryDecision::Retry { attempt: 2, .. }]
    ));
    let replay_witness = fs::read_to_string(&replay_witness).expect("read replay witness");
    assert_eq!(replay_witness.lines().count(), 1);

    let never_db = directory.path().join("never.sqlite");
    let never_witness = directory.path().join("never.witness");
    let never_status = Command::new(&executable)
        .arg("--exact")
        .arg("crash_child_never_replay")
        .arg("--nocapture")
        .env("ION_P1_CRASH_DB", &never_db)
        .env("ION_P1_CRASH_WITNESS", &never_witness)
        .env("ION_P1_CRASH_KIND", "never")
        .status()
        .expect("run never-replay child");
    assert_eq!(never_status.code(), Some(73));
    let mut never_store = PrototypeStore::open(&never_db).expect("reopen never-replay");
    assert!(matches!(
        never_store
            .recover_pending()
            .expect("recover never")
            .as_slice(),
        [RecoveryDecision::Indeterminate { .. }]
    ));
    let never_witness = fs::read_to_string(&never_witness).expect("read never witness");
    assert_eq!(never_witness.lines().count(), 1);
}
