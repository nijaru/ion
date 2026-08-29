from pathlib import Path

# The session module is topology only now. The reducer moved to operation long
# ago; keep no alias seam between those ownership boundaries.
session = Path("crates/ion-core/src/session/mod.rs")
text = session.read_text()
old = """//! Durable session ownership boundary.
//!
//! Session topology is passive semantic history plus active lane state. The
//! execution reducer lives in `operation`; the re-exports below are a narrow
//! migration seam for current runtime callers.

pub(crate) mod lane;
pub(crate) mod tree;

pub(crate) use crate::operation::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
"""
new = """//! Durable session topology: passive semantic history plus active lane state.
//! Operation execution and transition ownership live in `operation`.

pub(crate) mod lane;
pub(crate) mod tree;
"""
assert text == old, "session migration seam changed"
session.write_text(new)

runtime = Path("crates/ion-core/src/runtime/mod.rs")
text = runtime.read_text()
old = """use crate::session::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
"""
new = """use crate::operation::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
"""
assert text.count(old) == 1, "runtime session reducer import changed"
runtime.write_text(text.replace(old, new, 1))

store = Path("crates/ion-core/src/store/mod.rs")
text = store.read_text()
old = "use crate::session::{InboxKind, OperationState, SessionEntry};\n"
new = "use crate::operation::{InboxKind, OperationState, SessionEntry};\n"
assert text.count(old) == 1, "store session reducer import changed"
store.write_text(text.replace(old, new, 1))

for path, old, new in [
    ("crates/ion-core/src/context.rs", "use crate::session::SessionEntry;\n", "use crate::operation::SessionEntry;\n"),
    ("crates/ion-core/src/runtime/persistence.rs", "use crate::session::SessionEntry;\n", "use crate::operation::SessionEntry;\n"),
    ("crates/ion-core/src/agent_host.rs", "use crate::session::OperationState;\n", "use crate::operation::OperationState;\n"),
]:
    file = Path(path)
    text = file.read_text()
    assert text.count(old) == 1, f"expected one migration import in {path}"
    file.write_text(text.replace(old, new, 1))

compaction = Path("crates/ion-core/src/tests/compaction.rs")
text = compaction.read_text()
assert "crate::session::SessionEntry" in text, "compaction migration path not found"
compaction.write_text(text.replace("crate::session::SessionEntry", "crate::operation::SessionEntry"))

lib = Path("crates/ion-core/src/lib.rs")
text = lib.read_text()
old = """pub use operation::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition, TransitionError,
};
"""
new = """pub use operation::{InboxKind, OperationOutcome, OperationState, SessionEntry};
"""
assert old in text, "operation public export block changed"
lib.write_text(text.replace(old, new, 1))

design = Path("DESIGN.md")
text = design.read_text()
old = "9. Shrink/rename public Rust API and remove dead migration scaffolding now that ownership is stable. Begin with usage-proven accidental surface; do not rename coherent runtime/session concepts speculatively."
new = "9. Shrink/rename public Rust API and remove dead migration scaffolding now that ownership is stable. Accidental runtime/store exports are being removed first; session topology no longer aliases the operation reducer. Keep frontend-visible state/history contracts public and do not rename coherent runtime/session concepts speculatively."
assert old in text, "Step 9 design text changed"
design.write_text(text.replace(old, new, 1))
