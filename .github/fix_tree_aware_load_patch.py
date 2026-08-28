from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(text.replace(old, new))


for path in [
    "crates/ion-core/src/tests/crash_recovery.rs",
    "crates/ion-core/src/tests/print_mode.rs",
]:
    replace_once(
        path,
        '''    let pending = loaded
        .lane
        .state
        .pending_next_run
        .as_ref()
        .expect("durable next run");
''',
        '''    let pending = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane")
        .state
        .pending_next_run
        .as_ref()
        .expect("durable next run");
''',
        f"{path} main lane projection",
    )
