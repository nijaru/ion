from pathlib import Path

RUNTIME = Path("crates/ion-core/src/runtime/mod.rs")
FILES = [
    RUNTIME,
    Path("crates/ion-core/src/runtime/effects.rs"),
    Path("crates/ion-core/src/runtime/recovery.rs"),
]

FIELDS = [
    "operation_tool_calls",
    "draft_text",
    "draft_thinking",
    "assistant_frame_seq",
    "draft_calls",
    "draft_usage",
    "last_context_tokens",
    "last_prefix_fingerprint",
    "pending_compact",
    "overflow_retry_used",
    "last_step_was_compaction",
    "model_step",
    "live_tools",
]

text = RUNTIME.read_text()
anchor = '''struct ActiveOperation {
    machine: OperationMachine,
    /// Durable identity of the registry captured for the current model step.
    capability_snapshot: CapabilitySnapshot,
    /// Immutable registry captured for the current model step. Tool calls
    /// are admitted and executed against this registry, never a later live
    /// catalog generation.
    tool_registry: ToolRegistry,
    cancel: CancellationToken,
    state_seq: u64,
    /// The one in-flight effect intent, if any.
    open_effect: Option<EffectRecord>,
    /// Inbox items durably accepted but not yet applied.
    pending_steers: Vec<InboxId>,
}
'''
if text.count(anchor) != 1:
    raise SystemExit("ActiveOperation anchor mismatch")
residency = anchor + '''
/// Ephemeral execution state owned by the currently resident operation.
/// Keeping these fields together is a prerequisite for operation-addressed
/// residency: no draft, step counter, budget counter, or live tool state may
/// remain session-global once more than one lane can execute concurrently.
#[derive(Debug, Default)]
struct OperationResidency {
    operation_tool_calls: u32,
    draft_text: String,
    draft_thinking: String,
    assistant_frame_seq: u64,
    draft_calls: Vec<ToolCall>,
    draft_usage: Option<TokenUsage>,
    last_context_tokens: Option<u64>,
    last_prefix_fingerprint: Option<String>,
    pending_compact: Option<Option<String>>,
    overflow_retry_used: bool,
    last_step_was_compaction: bool,
    model_step: u64,
    live_tools: Vec<PendingTool>,
}
'''
text = text.replace(anchor, residency, 1)

# Rewrite the SessionRuntime field list while preserving documentation for
# fields that remain session- or lane-owned.
lines = text.splitlines(keepends=True)
start = next(i for i, line in enumerate(lines) if line == "struct SessionRuntime<P> {\n")
end = next(i for i in range(start + 1, len(lines)) if lines[i] == "}\n" and lines[i + 1].startswith("\nimpl<P: Provider> SessionRuntime"))
out = lines[: start + 1]
pending_docs: list[str] = []
inserted_live = False
for line in lines[start + 1 : end]:
    if line.startswith("    ///"):
        pending_docs.append(line)
        continue
    stripped = line.strip()
    field = stripped.split(":", 1)[0] if ":" in stripped else None
    if field in FIELDS:
        pending_docs.clear()
        continue
    out.extend(pending_docs)
    pending_docs.clear()
    out.append(line)
    if stripped.startswith("operation: Option<ActiveOperation>"):
        out.append("    operation_live: OperationResidency,\n")
        inserted_live = True
out.extend(pending_docs)
out.extend(lines[end:])
if not inserted_live:
    raise SystemExit("operation_live insertion failed")
text = "".join(out)

# Replace constructor assignments for the moved fields with one owned value.
lines = text.splitlines(keepends=True)
out = []
inserted_init = False
for line in lines:
    stripped = line.strip()
    field = stripped.split(":", 1)[0] if ":" in stripped else None
    if field in FIELDS and line.startswith("            "):
        continue
    out.append(line)
    if line == "            operation: None,\n":
        out.append("            operation_live: OperationResidency::default(),\n")
        inserted_init = True
if not inserted_init:
    raise SystemExit("operation_live constructor insertion failed")
RUNTIME.write_text("".join(out))

# All moved state is now explicitly owned by the residency object. These are
# intentionally mechanical replacements; the next patch can key the complete
# residency by OperationId without hunting for hidden singleton state.
for path in FILES:
    text = path.read_text()
    for field in FIELDS:
        text = text.replace(f"self.{field}", f"self.operation_live.{field}")
    path.write_text(text)
