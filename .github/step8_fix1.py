from pathlib import Path

path = Path("crates/ion-core/src/tests/structural_scope_recovery.rs")
text = path.read_text()
old = "use super::support::*;\n"
new = "use super::support::*;\nuse crate::{DurableEffect, HarnessProfile, DEFAULT_HARNESS_PROFILE_ID};\n"
if text.count(old) != 1:
    raise SystemExit(f"expected one structural_scope_recovery import anchor, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
print("Step 8 missing test imports added")
