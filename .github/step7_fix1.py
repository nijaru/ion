from pathlib import Path

path = Path("crates/ion-core/src/runtime/effects.rs")
text = path.read_text()
old = "        let (_, manifest) = self.current_context_manifest();\n"
new = "        let (_, _, manifest) = self.current_context_manifest(operation_id);\n"
if text.count(old) != 1:
    raise SystemExit(f"expected exactly one overflow manifest caller, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
