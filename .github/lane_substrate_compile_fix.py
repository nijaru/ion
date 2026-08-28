from pathlib import Path
import re


def regex_once(path: Path, pattern: str, replacement: str, label: str) -> None:
    text = path.read_text()
    rewritten, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    path.write_text(rewritten)


store = Path("crates/ion-core/src/store/mod.rs")
runtime = Path("crates/ion-core/src/runtime/mod.rs")
effects = Path("crates/ion-core/src/runtime/effects.rs")

# Keep the low-level store API public without leaking the crate-private Lane
# representation. The store constructs the total idle lane from stable public
# primitives and sends that internal value to its writer thread.
regex_once(
    store,
    r'''    pub async fn create_lane\(\n        &self,\n        session_id: SessionId,\n        lane: crate::session::lane::Lane,\n    \) -> Result<\(\), StoreError> \{\n        self\.request\(\|reply\| StoreCommand::CreateLane \{\n            session_id,\n            lane,\n            reply,\n        \}\)\n        \.await\n    \}''',
    '''    pub async fn create_lane(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        source_leaf: Option<EntryId>,
        model_ref: impl Into<String>,
    ) -> Result<(), StoreError> {
        let lane = crate::session::lane::Lane {
            name: lane_name.into(),
            state: crate::session::lane::State {
                leaf: source_leaf,
                current_operation: None,
                pending_next_run: None,
            },
            config: crate::session::lane::Config::new(model_ref),
        };
        self.request(|reply| StoreCommand::CreateLane {
            session_id,
            lane,
            reply,
        })
        .await
    }''',
    "public create-lane primitives",
)

# These main-only wrappers have no callers after admission/recovery became
# lane/operation addressed. Remove them rather than retaining compatibility
# aliases that would encourage new singleton assumptions.
regex_once(
    runtime,
    r'''\n    fn remove_main_operation\(&mut self\) -> Option<ResidentOperation> \{\n        self\.remove_operation\(self\.main_resident_id\(\)\?\)\n    \}\n''',
    "\n",
    "remove main operation wrapper",
)
regex_once(
    runtime,
    r'''\n    fn main_leaf\(&self\) -> Option<EntryId> \{\n        self\.lane_leaf\(crate::session::lane::MAIN\)\n    \}\n''',
    "\n",
    "remove main leaf wrapper",
)
regex_once(
    runtime,
    r'''\n    fn emit_terminal_state\(&mut self, state: &OperationState\) \{\n        if let Some\(operation_id\) = self\.main_resident_id\(\) \{\n            self\.emit_terminal_state_for\(operation_id, state\);\n        \}\n    \}\n''',
    "\n",
    "remove main terminal wrapper",
)
regex_once(
    effects,
    r'''\n    pub\(crate\) async fn fail_operation_on_persistence\(&mut self, err: StoreError\) \{\n        let Some\(operation_id\) = self\.main_resident_id\(\) else \{\n            error!\(session = %self\.session_id, %err, "persistence failed with no active main operation"\);\n            return;\n        \};\n        self\.fail_operation_on_persistence_for\(operation_id, err\)\n            \.await;\n    \}\n''',
    "\n",
    "remove main persistence wrapper",
)
