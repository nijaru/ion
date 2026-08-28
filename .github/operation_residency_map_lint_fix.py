from pathlib import Path
import re

effects = Path("crates/ion-core/src/runtime/effects.rs")
text = effects.read_text().replace("operation_id: operation_id,", "operation_id,")
effects.write_text(text)

runtime = Path("crates/ion-core/src/runtime/mod.rs")
text = runtime.read_text()
text, count = re.subn(
    r'''\n    fn main_active_mut\(&mut self\) -> Option<&mut ActiveOperation> \{\n        self\.main_resident_mut\(\)\.map\(\|resident\| &mut resident\.active\)\n    \}\n''',
    "\n",
    text,
    count=1,
)
if count != 1:
    raise SystemExit(f"main_active_mut cleanup: expected 1 match, found {count}")
runtime.write_text(text)
