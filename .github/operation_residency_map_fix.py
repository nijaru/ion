from pathlib import Path

path = Path(".github/operation_residency_map_patch.py")
text = path.read_text()
if "import re\n" not in text:
    text = text.replace("from pathlib import Path\n", "from pathlib import Path\nimport re\n", 1)

start = text.index("# Mechanical projection of the old singleton active slot through `main`.")
end = text.index("# Ephemeral state now lives beside the operation core", start)
mechanical = """# Mechanical projection of the old singleton active slot through `main`.
# Match across formatting/newlines so the migration is semantic rather than
# dependent on rustfmt's current wrapping choices.
for path in (runtime, effects, recovery):
    text = path.read_text()
    text = re.sub(r"self\\.operation\\s*\\.as_ref\\(\\)", "self.main_active()", text)
    text = re.sub(r"self\\.operation\\s*\\.as_mut\\(\\)", "self.main_active_mut()", text)
    text = re.sub(r"self\\.operation\\s*\\.is_some\\(\\)", "self.main_active().is_some()", text)
    text = re.sub(r"self\\.operation\\s*\\.is_none\\(\\)", "self.main_active().is_none()", text)
    text = re.sub(r"self\\.operation\\s*\\.clone\\(\\)", "self.main_active().cloned()", text)
    text = text.replace("match &self.operation", "match self.main_active()")
    text = text.replace("let Some(active) = &self.operation else", "let Some(active) = self.main_active() else")
    text = text.replace("if let Some(active) = &self.operation", "if let Some(active) = self.main_active()")
    text = text.replace("if let Some(active) = &mut self.operation", "if let Some(active) = self.main_active_mut()")
    text = text.replace("self.operation = Some(staged);", "self.install_active(staged);")
    text = text.replace("self.operation.take();", "self.remove_main_operation();")
    path.write_text(text)

"""
text = text[:start] + mechanical + text[end:]

start = text.index("# Ephemeral state now lives beside the operation core")
end = text.index("# Immutable main-lane cache/snapshot helpers", start)
ephemeral = """# Ephemeral state now lives beside the operation core in the same residency.
# Mutation-heavy paths use the mutable projection first; immutable methods are
# corrected explicitly below.
for path in (runtime, effects, recovery):
    text = path.read_text()
    text = re.sub(
        r"self\\.operation_live\\s*\\.\\s*",
        'self.main_live_mut().expect("main operation residency exists").',
        text,
    )
    path.write_text(text)

"""
text = text[:start] + ephemeral + text[end:]

start = text.index("# Restore builds the resident privately before publication")
end = text.index("# New acceptance publishes the residency once;", start)
restore = """# Restore builds the resident privately before publication so frame state and
# lane ownership appear together at one in-memory publication point. Delimit
# by semantic anchors because the frame block is frequently rustfmt-wrapped.
text = runtime.read_text()
restore_hint = text.index('"reopened an open operation; recovery is Step 3 work"')
active_start = text.index("            let active = ActiveOperation {", restore_hint)
frame_start = text.index(
    "            if matches!(payload.state, OperationState::AssistantEffectPending)",
    active_start,
)
publish_start = text.index("            if self.operation.replace(active).is_some() {", frame_start)
frame_block = text[frame_start:publish_start]
frame_block = re.sub(
    r'self\\.main_live_mut\\(\\)\\.expect\\("main operation residency exists"\\)\\.\\s*',
    "resident.live.",
    frame_block,
)
replacement = (
    "            let mut resident = ResidentOperation::new(operation.lane_name.clone(), active);\\n"
    + frame_block
    + "            if self.operations.insert(operation.id, resident).is_some() {\\n"
)
publish_line_end = text.index("\\n", publish_start) + 1
text = text[:frame_start] + replacement + text[publish_line_end:]
runtime.write_text(text)

"""
text = text[:start] + restore + text[end:]

start = text.index("# New acceptance publishes the residency once;")
end = text.index("# A few direct singleton forms", start)
start_active = """# New acceptance publishes the residency once; its live state starts total and
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

"""
text = text[:start] + start_active + text[end:]
path.write_text(text)
