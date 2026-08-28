from pathlib import Path

path = Path("crates/ion-core/src/runtime/mod.rs")
text = path.read_text()
old = '''    fn operation_lane_live(&self, operation_id: OperationId) -> Option<&LaneResidency> {
        self.lane_live(self.operation_lane_name(operation_id)?)
    }

'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"operation_lane_live cleanup: expected 1 match, found {count}")
path.write_text(text.replace(old, "", 1))
