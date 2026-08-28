from pathlib import Path

path = Path("crates/ion-core/src/tests/multi_lane.rs")
text = path.read_text()
old = '''                RuntimeEvent::OperationFinished { operation_id, .. }
                    if operation_id == main_operation || operation_id == worker_operation =>
                {
                    if !finished.contains(&operation_id) {
                        finished.push(operation_id);
                    }
                }
'''
new = '''                RuntimeEvent::OperationFinished { operation_id, .. }
                    if (operation_id == main_operation || operation_id == worker_operation)
                        && !finished.contains(&operation_id) =>
                {
                    finished.push(operation_id);
                }
'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"finished-event guard: expected 1 match, found {count}")
path.write_text(text.replace(old, new, 1))
