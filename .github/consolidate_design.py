from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    return text.replace(old, new, 1)


path = Path("DESIGN.md")
text = path.read_text()
text = replace_once(text, "**Date:** 2026-08-27", "**Date:** 2026-08-28", "design date")

text = replace_once(
    text,
    '''Reference weighting:\n\n1. **Pi 2** is the primary practical and architectural reference for the harness. Its conversation-tree/lane model, one-writer session semantics, provision-before-effect discipline, forks, cache-friendly history growth, and explicit-state redesign are the leading alpha.\n2. **DeepSeek Harness / Cordis** is the strongest independent cross-check for scoped ownership, lifecycle, and shared infrastructure with agent-local visibility.\n3. **Codex** is a high-value independent production reference for family-scoped agent control, lineage, budgets, messaging, sandbox/policy boundaries, and Rust implementation choices.\n4. **Grok Build** is useful concrete Rust evidence for background/resumable agents, worktree isolation, and concurrency.\n5. **Prime Agent** is a pressure test for recursive and long-running compositions, not a source of core semantics.\n\nDo not transliterate another system's TypeScript API, physical persistence format, compatibility baggage, or framework vocabulary into Rust.\n''',
    '''Reference weighting:\n\n1. **Pi 2** is the primary durable-session reference: parent-linked history, lanes as active cursors, one writer with one open operation per lane, parallel slow effects, forks, provision-before-effect identity, replay/reconciliation, and explicit total lane/operation state. Ion follows the logical model rather than Pi's TypeScript API or JSONL persistence.\n2. **DeepSeek Harness / Cordis** is the strongest ownership/composition cross-check: agent-scoped visibility, lifecycle-owned registrations/resources, private setup followed by one publication point, rollback on failed admission, and one authority per independent fact.\n3. **Codex** is the strongest production constraint on multi-agent control: family-scoped authority, execution/rollout budgets, control lineage separate from history lineage, retained identity separate from live capacity, cancellation, recovery, and headless lifecycle visibility.\n4. **Zed / Agent Client Protocol (ACP)** is the primary interoperability signal for the client/agent boundary: explicit session lifecycle, prompt/update/cancel flow, capability negotiation, permissions, and ordered resume updates.\n5. **VS Code Agent Host / Agent Host Protocol (AHP)** is complementary evidence for a persistent host that owns sessions independently of editor clients, supports reconnect from snapshot plus ordered actions, and can serve multiple local or remote clients.\n6. **Grok Build** is strong inspectable Rust evidence for background/resumable agents, worktree isolation, transition-driven waiting, per-agent workspace identity, and host-stamped lifecycle facts.\n7. **Cloudflare Agents** reinforces the identity/residency distinction: durable agent identity must not depend on an always-running process.\n8. **Prime Agent** and **Headlong** are experimental stress tests for recursive, persistent, message-driven, and long-running compositions. Their benchmark or shared-mind choices are not core architecture authority.\n\nCursor and Warp/Oz are product/UX evidence for parallel sessions, worktrees, remote agents, and unified agent views. Factory Droid and Amp are useful closed-source references only where public docs or blogs expose concrete observable semantics; do not infer their internals. OpenCode remains low-weight because repeated rewrites weaken convergence claims. Gemini CLI is not an architectural reference.\n\nDo not transliterate another system's TypeScript API, physical persistence format, compatibility baggage, or framework vocabulary into Rust.\n''',
    "reference weighting",
)

text = replace_once(
    text,
    '''LaneState\n  leaf: Option<EntryId>\n  current_operation: Option<OperationId>\n  pending_next_run: Vec<QueuedInput>\n''',
    '''LaneState\n  leaf: Option<EntryId>\n  current_operation: Option<OperationId>\n  pending_next_run: Option<NextRun>\n''',
    "lane state shape",
)
text = replace_once(
    text,
    '''The current `enqueue` API is migration vocabulary. Its target meaning is **queue next-run input**, not create another `Accepted` operation behind an active one.\n\nWhen a run is accepted, the transition atomically captures the lane's pending next-run input, appends the resulting semantic entries, creates the immutable operation record and first total state, and installs `current_operation`.\n''',
    '''`next_run` queues at most one lane-owned input outside any operation. It provisions the semantic `EntryId` immediately, but no `OperationId` exists until the lane actually accepts that run.\n\nWhen a run is accepted, the transition atomically captures the lane's pending next-run input, preserves its provisioned semantic entry identity, provisions the `OperationId`, appends the resulting semantic entries, creates the immutable operation record and first total state, installs `current_operation`, and clears the captured pending input.\n''',
    "next run semantics",
)
text = replace_once(
    text,
    '''`SessionEntry::ModelChanged` is migration scaffolding and must disappear once lane configuration is authoritative.\n''',
    '''Model selection exists only as authoritative lane configuration; it is not semantic conversation history.\n''',
    "model history cleanup",
)
text = replace_once(
    text,
    '''The mutable side is an append-only sequence of **total** state revisions. One latest revision plus immutable acceptance must be sufficient to determine what may happen next.\n''',
    '''The mutable side is a **total** latest operation state. One latest state plus immutable acceptance must be sufficient to determine what may happen next. Historical revisions may be retained when audit, debugging, or evaluation earns them, but recovery never depends on folding partial history.\n''',
    "operation total state",
)
text = replace_once(
    text,
    '''Historical revisions may be retained for audit and debugging, but no prior revision may be required to fill missing fields in the current state.\n\nCancellation is orthogonal control over active workflow state rather than an artificial workflow phase.\n''',
    '''No prior revision may be required to fill missing fields in the current state.\n\nCancellation is orthogonal control over active workflow state rather than an artificial workflow phase.\n''',
    "operation revision followup",
)
text = replace_once(
    text,
    '''- operation-state revisions are append-only total records indexed so the latest revision is cheap to load;\n''',
    '''- the latest total operation state is directly readable, with revision history retained only where it provides real audit/debug/evaluation value;\n''',
    "sqlite operation state",
)

text = replace_once(
    text,
    '''### 13.1 Lineage\n''',
    '''### 13.1 Durable identity and residency\n\nAn agent address is durable semantic identity. It must not embed a Tokio task, process incarnation, terminal/client attachment, execution permit, or foreground/background choice. The host provisions authoritative identity before work begins; worker code does not self-report host-owned facts such as identity, control parentage, workspace identity, or delivery mode.\n\nResidency describes where an admitted agent is currently executable: an in-process session task, a future local worker process, a future remote host, or inactive/hibernated durable state. Residency is runtime state, not identity or conversation semantics.\n\n### 13.2 Lineage\n''',
    "agent identity residency",
)
text = replace_once(text, "### 13.2 Family-scoped control\n", "### 13.3 Family-scoped control\n", "family heading")
text = replace_once(
    text,
    '''A root/session family may own a control plane above individual lanes/sessions for:\n\n- stable agent identity/path;\n- lane/fork/fresh/external spawn admission;\n- direct messaging;\n- observe/wait/status;\n- cancel/interrupt/resume;\n- descendant accounting;\n- concurrency/depth/token/time budgets;\n- background completion routing;\n- recovery of admitted children.\n\nThis is family-scoped, not one process-global mutable registry. `ChildManager` and the test-only delegation path are migration scaffolding rather than target architecture.\n\nSwarm/reviewer/supervisor strategies are ordinary compositions over this API, not privileged phases in the model loop.\n''',
    '''A root/session family owns one control authority above individual lanes/sessions for:\n\n- stable agent identity/path and control parentage;\n- retained/admitted descendants;\n- lane/fork/fresh/external spawn admission;\n- execution permits and concurrency/depth/token/time/rollout budgets;\n- direct messaging;\n- observe/wait/status;\n- cancel/interrupt/resume and cancellation ownership;\n- deterministic recovery/reattachment;\n- background completion routing.\n\nThis is family-scoped, not one process-global mutable registry. Separate roots must not accidentally share namespaces, budgets, or cancellation trees. Retained identity and active execution capacity are separate: a completed agent may remain observable after releasing its execution permit. `ChildManager` and the test-only delegation path are migration scaffolding rather than target architecture.\n\nSwarm/reviewer/supervisor strategies are ordinary compositions over this API, not privileged phases in the model loop.\n\n### 13.4 Admission, waiting, and messaging\n\nSpawn is admission-first:\n\n1. provision durable identity and requested topology;\n2. privately construct and validate scoped capabilities/resources;\n3. durably publish/admit exactly once;\n4. return the stable address promptly;\n5. start or attach execution residency;\n6. observe completion separately.\n\nFailed or racing setup rolls back private resources. Foreground behavior is `spawn + wait`; background behavior is spawn without that wait. Completion is not part of identity creation.\n\nWaiting wakes from authoritative state transitions rather than polling. One/any/all are distinct semantics; cancellation/deadline is explicit; dropping a waiter cannot consume completion or mutate durable agent state.\n\nUser prompts, agent-to-agent messages, background completion, schedules, heartbeats, and future external events converge on durable session input rather than mutating another session's projected context directly. Delivery policy decides whether an input steers active work, becomes a follow-up, becomes the lane's next run, or remains informational.\n''',
    "family control expansion",
)

text = replace_once(
    text,
    '''A likely eventual public shape is one session-oriented harness plus lane-targeting command surfaces. Exact names should be chosen once the topology migration is real; do not proliferate temporary `Runtime*`, `Manager`, `Service`, or `Handle` types merely to sketch it.\n''',
    '''A likely eventual public shape is one session-oriented harness plus lane-targeting command surfaces. Exact names should be chosen once the topology migration is real; do not proliferate temporary `Runtime*`, `Manager`, `Service`, or `Handle` types merely to sketch it.\n\n### 16.1 Client/host boundary\n\nThe execution host/session writer is authoritative. TUI, print/JSON, ACP, and future remote or multi-client protocols project an initial snapshot plus ordered runtime/session events/actions and send commands carrying stable semantic IDs. A client disconnect must not implicitly cancel durable work. Settle this boundary before redesigning the TUI so frontend ownership never leaks into execution semantics.\n''',
    "client host boundary",
)

old_order = '''## 19. Implementation order\n\n1. Make `EntryId` independently provisioned before persistence and complete the parent-linked tree + durable `main` lane store slice.\n2. Make lane configuration authoritative and remove `ModelChanged` from semantic history.\n3. Add total lane state (`leaf`, `current_operation`, `pending_next_run`).\n4. Separate immutable operation acceptance from append-only total operation-state revisions and bind each operation to a lane/source leaf.\n5. Change current queue behavior so `enqueue` becomes lane-owned next-run input rather than a second queued operation.\n6. Map current CLI/ACP behavior onto `main`, then allow multiple lanes and concurrent slow lane effects under the one session writer.\n7. Finish typed effect writers/codecs and remove raw effect-JSON knowledge from runtime/store control flow.\n8. Generalize child creation into family-scoped lane/fork/fresh agent control with reconcilable identities and explicit lineage.\n9. Add lifecycle-owned contribution scopes only from concrete tool/context/extension needs.\n10. Shrink/rename the public Rust API once ownership is stable; then perform the separate Rust/code-hygiene pass.\n11. Establish/expand `ion-eval` before adding speculative long-horizon or swarm policy.\n\nResearch from here is question-driven at implementation boundaries, not another broad framework survey.\n'''
new_order = '''## 19. Current implementation checkpoint\n\nThe current workstream has already established:\n\n- authoritative UUIDv7 `EntryId` provisioning before persistence;\n- the parent-linked conversation tree and durable lane rows;\n- total lane state/configuration (`leaf`, `current_operation`, one `pending_next_run`, model config);\n- `next_run` reserving only semantic entry identity while `OperationId` is created at acceptance;\n- lane-addressable operation admission with immutable accepting lane and exact source leaf;\n- later commits deriving lane ownership from immutable origin;\n- model selection exclusively in lane config;\n- tree-aware recovery that loads and validates every durable lane over the shared tree, with pending durable inbox restored per operation;\n- Rust 1.98.0 as the workspace/toolchain/CI contract.\n\nStorage and recovery can now represent multiple lanes. Live execution still projects `main` and owns singleton active-operation/draft/effect state; that is the active boundary.\n\n## 20. Implementation order\n\n1. Retain the full conversation tree and all durable lane state in the live session owner while preserving the current `main` projection.\n2. Replace singleton active-operation/draft/effect residency with operation-addressed, lane-owned runtime state; route provider/tool signals by stable operation identity.\n3. Add the runtime-owned lane admission surface together with its durable transaction, then allow concurrent slow effects across lanes under the one session writer.\n4. Introduce family-scoped agent control with admission-first identity, separate retained registry/execution permits, explicit wait semantics, cancellation ownership, and deterministic reattachment.\n5. Replace child-only topology with lane/fork/fresh agent admission. Add worktree/remote topology only when a concrete owner exists.\n6. Add durable agent messaging/background completion through the common session-input path.\n7. Add scoped capability publication/teardown around agent creation/resume.\n8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants.\n9. Shrink/rename public Rust API and remove dead migration scaffolding once ownership is stable.\n10. Redesign the TUI only after the authoritative session/agent host contract is coherent, with ACP as a first-class client boundary.\n\nResearch from here is question-driven at concrete implementation boundaries, not another broad framework survey.\n'''
text = replace_once(text, old_order, new_order, "current checkpoint and order")
path.write_text(text)
