from pathlib import Path

path = Path("crates/ion-core/src/runtime/mod.rs")
text = path.read_text()
old = '''            self.start_active(active);
            let operation_id = active.machine.operation_id();
            self.advance(operation_id).await;'''
new = '''            let operation_id = active.machine.operation_id();
            self.start_active(active);
            self.advance(operation_id).await;'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"next-run move order: expected 1 match, found {count}")
path.write_text(text.replace(old, new, 1))
