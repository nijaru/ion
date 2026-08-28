from pathlib import Path

path = Path(".github/operation_residency_map_patch.py")
text = path.read_text()
start = text.index("# New acceptance publishes the residency once;")
end = text.index("# A few direct singleton forms", start)
replacement = '''# New acceptance publishes the residency once; its live state starts total and
# empty instead of resetting a parallel session-global object. Earlier
# mechanical rewrites intentionally change the exact body, so delimit this
# function by stable semantic anchors rather than a formatting-sensitive blob.
text = runtime.read_text()
fn_start = text.index("    fn start_active(&mut self, active: ActiveOperation) {")
emit_start = text.index("        self.emit(RuntimeEvent::OperationStarted {", fn_start)
old_prefix = text[fn_start:emit_start]
if "self.operation = Some(active);" not in old_prefix:
    raise SystemExit("start_active publication anchor missing")
new_prefix = '''    fn start_active(&mut self, active: ActiveOperation) {\n        let operation_id = active.machine.operation_id();\n        let prompt = active.machine.prompt().to_owned();\n        let previous = self.operations.insert(\n            operation_id,\n            ResidentOperation::new(crate::session::lane::MAIN, active),\n        );\n        debug_assert!(previous.is_none(), "operation residency identity is unique");\n'''
text = text[:fn_start] + new_prefix + text[emit_start:]
runtime.write_text(text)

'''
path.write_text(text[:start] + replacement + text[end:])
