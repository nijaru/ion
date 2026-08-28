from pathlib import Path

path = Path("crates/ion-core/src/runtime/mod.rs")
text = path.read_text()
old = '''    /// The durable seq of the first in-memory transcript entry.\n    fn first_entry_seq(&self) -> u64 {\n        self.main_branch_records()\n            .first()\n            .map_or(self.next_entry_seq, |record| record.seq)\n    }\n\n'''
if text.count(old) != 1:
    raise SystemExit(f"first_entry_seq cleanup: expected 1 match, found {text.count(old)}")
path.write_text(text.replace(old, "", 1))
