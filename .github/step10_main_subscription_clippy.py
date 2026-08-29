from pathlib import Path

path = Path("crates/ion-core/src/runtime/mod.rs")
text = path.read_text()
old = """        let is_main_event = event.operation_id().map_or(true, |operation_id| {
            self.operation_lane_name(operation_id) == Some(crate::session::lane::MAIN)
        });
"""
new = """        let is_main_event = event.operation_id().is_none_or(|operation_id| {
            self.operation_lane_name(operation_id) == Some(crate::session::lane::MAIN)
        });
"""
assert text.count(old) == 1, "main-event routing context changed"
path.write_text(text.replace(old, new, 1))
