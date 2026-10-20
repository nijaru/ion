# Rust embedding

Embed the existing engine through Host shared control. Coding is a preset, not ambient authority for every application.

[Start](../README.md) · [Terminal](terminal.md) · [Configuration](configuration.md) · [Tools](tools.md) · [Automation](automation.md) · [Rust embedding](embedding.md)

## Application setup and shared control

The runnable [embedded example](../crates/ion-app/examples/embedded.rs) submits one
prompt through shared Host session control:

```sh
cargo run --locked -p ion --example embedded -- --help
cargo run --locked -p ion --example embedded -- \
  --cwd . --session /tmp/ion-embedded.sqlite \
  --provider xiaomi --model mimo-v2.6-flash 'Explain this repository'
```

Choose an available route with matching credentials in the normal Ion config
or environment; provider usage may be billed. Reuse the same Session path and
working directory to continue. Ctrl-C requests stop and joins the operation;
all post-Session errors pass through shutdown before the runtime exits. The
example prints only the committed answer using fallible output, restores the
saved reasoning preference, and connects no MCP servers. It is an SDK lifecycle
sample, not a replacement for the terminal or RPC client.

`CodingApplication::new(host, tools).setup(session, selected, reasoning, questions)` supplies
the coding preset. It loads coding instructions/resources and native tools; the
optional `tools` source adds or overrides registrations, not a complete allowlist.
`with_tool_selection(ToolSelection::include(...).with_exclude(...))` narrows the
model-visible loadout; `with_read_only_tools()` selects the read-only preset.
`ToolSelection::include([])` selects no tools; `ToolSelection::all()` leaves
inclusion unrestricted. Selection filters model declarations, discovery and the
Code Mode namespace, and
managed children inherit the parent selection. They are application loadout
policies, not OS sandboxes or authority grants.
Consume the setup with `into_control::<M>()` to create `SessionControl<M>`, the same
owner used by terminal, headless, RPC and this example—not another model/tool loop.
It handles preparation, steering, follow-ups, settlement and idle replacement.

### Restricted applications and questions

For restricted/non-coding embedding, implement `Application`: `resources(cwd)`
returns explicit resources, and `runtime(cwd, model)` returns the complete agent
configuration. `Resources::new(instructions)` does no filesystem discovery;
`ApplicationRuntime::new(agent)` grants neither direct shell nor local-image-path
access. Encoded/normalized images still require the model's image capability.
Native input is an explicit grant, not a sandbox or path allowlist.

Construct `SessionSetup::new(session, catalog, application, model, reasoning, questions)`.
Pass `None` for no question interaction, or an owned `QuestionCustody::new()` in
`Some` for a real consumer. `Application::runtime` receives that optional port as
an `Arc`; custom recipes explicitly register `QuestionTools` if they offer it.
`SessionControl::attach_question_responder()` returns the non-clone consumer
lease. Pull its pending request, then call `answer` or `cancel` with the exact
request ID. Host invalidations wake shared control; complete authorized snapshots
also expose the current pending request, not answer authority. Idle model/Session
replacement retains the consumer while invalidating old request IDs.
Omitted model/reasoning restore the saved preferences; a fresh Session uses the
application's model default. The recipe remains attached through reload and
replacement. Clone retains the resolved runtime rather than resolving a route
name again. Explicit model selection and reopening resolve through the recipe;
unavailable requested identities fail rather than silently selecting a default.
Create a workspace-free Session with `CodingSession::create(path, None)` and its
catalog with `SessionCatalog::new(state_root, None)`—no dummy directory is needed.
The coding executable rejects workspace-free Sessions for coding operations;
inspection can still represent their `cwd` as null.

The credential-free [restricted example](../crates/ion-app/examples/restricted.rs)
uses a synthetic model and one custom counting tool through this same API:

```sh
cargo run --locked -p ion --example restricted -- "$(mktemp -d)"
```

It checks actual requests, one custom-tool effect and delivery reconciliation
after reopen. After building the example, `python3 scripts/smoke_recovery.py` also
kills an owned instance after its effect and explicitly continues the original
Turn with the missing observation still unknown. These are synthetic SDK checks,
not live-provider or service qualification. Applications must authenticate callers
and authorize control; selecting tools does not isolate tenants or host effects.

The credential-free [delegation example](../crates/ion-app/examples/delegation.rs)
uses separate restricted parent and child applications through shared control:

```sh
cargo run --locked -p ion --example delegation -- "$(mktemp -d)"
cargo run --locked -p ion --example delegation -- "$(mktemp -d)" --fork
cargo run --locked -p ion --example delegation -- "$(mktemp -d)" --background --fork
```

After building it, `python3 scripts/smoke_delegation.py` also kills an owned
process after a synced child host effect, for fresh/forked foreground children
and background children whose parent has already joined. Explicit parent-Turn
continuation (foreground) and fresh-work recovery preserve the original child
identity and unknown effects without replay or a replacement child. Focused faults
cover transfer/relationship commits, both cancellation scopes, unread custody,
concurrent joins, reader-independent failure/panic and Code Mode error catching.
These qualify bounded offline SDK ownership/recovery, not live-model delegation,
service scale or remote-effect quiescence.

### Input custody and durable admission

Use `submit(Delivery, UserInput, metadata)` for input. `Prompt` expands resources
once and normalizes attachments; an already-prepared `Message` stays literal.
Refusal returns the original input and metadata. The returned `InputId` means
Host custody, not durable acceptance. Client metadata requires `Serialize + Send`
but not `Clone`; it never enters model content or Session records.

For durable direct delivery, use `submit_request(&RequestKey, Message, metadata)`.
Create the key with `RequestKey::new(scope, id)`; both opaque, case-sensitive
components are nonblank, at most 256 bytes and contain no control characters.
The application authorizes the namespace—keys are not authentication tokens.
`RequestAdmission::Started { id, receipt }` is returned only after the key and
literal input commit together with Turn admission. Normal `Accepted`/`Settled`
updates still carry this attempt's metadata. Redelivery of the same key and exact
message returns `Existing { receipt, metadata }` without a task, model request or
tool dispatch, including while busy, after shutdown or after reopen. Different
input is an error; changing current configuration does not retarget accepted work.

`session.request_receipt(&key)` reads acceptance without reconstructing an
application recipe. A receipt names the logical Session and Turn, with its latest
committed end reason. `end: None` does not prove work is running; lost observations
remain unknown. Reconciliation is passive. New explicit work closes interrupted
history conservatively; it does not rerun the old request. Prepare templates and
local files before constructing this literal message. Pending queues, client
metadata and live services are not made durable. Ordinary CLI/TUI submits and RPC
wire command IDs remain unkeyed; RPC `submit_request` uses a separate explicit key
for literal text. Request deduplication is not exactly-once tool execution.

### Explicit interrupted-Turn continuation

For explicit interrupted-Turn continuation, configure the application's agent with
`with_recovery_scope(AuthorityScope::new(versioned_reference)?)` **before admission**.
The reference identifies the actual application/principal, environment, model
route and grants. It is a trusted application assertion—not proof that equal
names, schemas or hashes imply equal credentials or executors. Do not put secrets
in it. On reopen, resolve and authorize that reference and supply the corresponding
`Application` recipe; Ion does not reconstruct live credentials or executors.

`session.recovery_state()` passively returns the saved authority and a
`RecoveryPoint { session, turn, checkpoint }` for an opted-in unfinished Turn.
It does not prove liveness or that old effects stopped. Explicitly choose whether
to proceed despite uncertainty, then call `control.resume_turn(point, metadata)`.
Core checks the exact checkpoint, scope, model preference and declaration binding
before committing continuation and unknown settlements atomically. The same loop
starts its next model step; it never reruns old calls or reconstructs a guest.
Changed instructions, controls, callable definitions or scope are rejected. The
opt-in mode pins executable routes for the whole Turn, while deferred discovery
can expand model exposure. Prepare dynamic sources before admission; refreshes
and changed authority require new work, not a silent rebind.

`RecoveryAdmission` returns a local operation ID and its committed checkpoint.
No new input or Turn is accepted; settlement returns this attempt's metadata in
`Settlement.metadata`, separately from accepted steering. Reusing an old point is
stale, not another dispatch. After another process loss, a fresh continuation
requires an explicit decision against the new checkpoint. Committed-ended Turns
cannot be resumed, and context copies inherit no continuation grant. Redelivering
a request key still only reconciles—it never resumes work. Reconcile committed
history on `TurnResumed`; it is not a fresh-input event. A later model-issued call
can still duplicate an earlier uncertain effect: this is not exactly-once execution.

### Driving and shutting down owners

Drive `next()` while handling client I/O. Reliable updates distinguish local
`Dispatched` correlation, durably `Accepted` input with a Turn ID, `Returned`
originals and `Settled` outcomes/metadata. `state()` derives activity and queue
counts from this owner. Borrowing/cancelling a `next()` wait does not stop work.
Keep polling to advance preparation and queued follow-ups; do not implement
another preparation queue or infer acceptance from rendered progress.

`request_stop()` is not completion. Await `shutdown()` before destroying the
runtime, including on client errors: it closes admission, stops and joins
preparation/Core work and returns remaining updates and cleanup errors. Cancelling
that wait leaves it closed and joinable; Drop can request stop but cannot join.
Drain returned inputs and settlements before replacing a Session or model.

A coding setup also retains an `ApplicationOwner`, available through
`control.owner()`. Closing one Session does not close this longer-lived owner:
request its shutdown, join control, then join the application before releasing
MCP or other captured services and before potentially blocking final output.
It closes both delegated-agent and native-process admission paths before joining
either family. `Application::owner()` must return the same retained owner, not a
new owner on each lookup. Restricted recipes default to no application owner;
`ApplicationRuntime::new` contains no process supervisor. A recipe installing
`ProcessTools` must explicitly retain its supervisor in the runtime's `processes`
field for delegated-job cleanup. Session history, cloned tool declarations and
saved IDs cannot reconstruct these live resources.

`observe()` is passive. Lag produces an explicit gap: discard the partial stream
and reconcile committed Session facts, rather than joining a surviving suffix to
old deltas. Reader detachment does not cancel execution; a service can retain
control while clients detach. This is a progress stream, not a complete
snapshot API by itself. Foreground clients explicitly stop/join on I/O loss.
MCP connection cleanup remains a separate `McpTools::shutdown()` obligation;
await it to completion. It retires all peers before joining services and owned
root processes, so unanswered calls do not require caller cancellation to close.
Unanswered effects remain unknown, never replayed. Already observed results remain
owned by their calling operations, including queued conversion work; connection
shutdown does not replace the operation stop/join obligation.

### Service attachment and snapshots

For a service, retain `SessionControl` outside connection scope and drive
`maintain()` while busy. It advances the same preparation, joining and follow-ups
as `next()`, but keeps reliable updates and their capacity reservations in Host
custody. Passive readers cannot acknowledge or evict results. `take_update()` (or
`next()`) explicitly transfers originals/metadata and settlements to the controlling
application and releases capacity; it is not a transport acknowledgement. If that
application promises later recovery, it must retain or persist the transfer.
Full custody refuses new admission, not stop/join. There is no receipt ring that
silently expires returned originals, nor unlimited/cross-restart metadata retention.

After authorizing inspection, call `attach(project_metadata)` for a complete
`ControlSnapshot` and a passive observer, registered before the read. `snapshot()`
refreshes the view without consuming results. It includes committed Session facts,
retained operation/preparation/queue state, pending withdrawal/acknowledgement,
reliable returns/settlements and cleanup failures. Projection borrows metadata;
`M` needs neither Clone, Sync nor Deserialize. These privileged views include raw
images, opaque replay and private shell context: **do not serialize them blindly
to a network**. Authorize the full Session/queue scope and select disclosure.

`SnapshotCursor` identifies the live owner and Session, orders replacement reads
and records their committed fact count. A new owner has a new epoch, even after
reopening the same Session. Custody is sampled before committed facts: all Host
acknowledgements are covered, but facts may be ahead of callback/join processing.
An in-flight input is not proof of absent admission; a retained task is not proof
it is physically running. This is a causal view, not an atomic Core/Host transaction.
Replace the entire snapshot; its cursor does not order activity deltas. Lag,
reconnect or an owner/Session change requires a fresh view, never suffix-splicing.
Bound concurrent full-view requests: history/materialization has no aggregate
memory quota. Keep transport writes out of the owner driver.

`request_shutdown()` closes admission and requests stop synchronously.
`join_shutdown()` finishes cleanup but retains results/failures for inspection;
`state().shutdown_complete` marks completion of that protocol, not error-free
cleanup or remote quiescence. Cancelling its wait retains custody. `shutdown()`
also transfers the remaining updates/errors to its caller.

The credential-free [service example](../crates/ion-app/examples/service.rs) exercises
late join, permission denial, slow/dropped readers, gaps, reconnect, explicit result
retention, stop and joined shutdown through bounded in-process transport requests:

```sh
cargo run --locked -p ion --example service -- "$(mktemp -d)"
```

It is synthetic SDK qualification, not a network authentication implementation,
a mandatory daemon or evidence of service scale or live-model adherence.

For lower-level integration, `ion-core::CodingAgent` and `CodingSession` expose
retained `Operation` handles and abstract model/tool contracts;
`Host::agent(selected, tool_set)` combines a selected HTTP route with a complete
caller-owned `ToolSet`, without adding coding tools or Code Mode. Custom `ModelService`
and tool sources can be assembled inside an `Application` and use shared control. Starting a Core operation reserves execution
immediately and rejects concurrent starts. Retain/join each handle:
dropping an owning wait requests stop, while timing out a borrowed `&mut operation`
only stops waiting. `CodingSession::shutdown()` joins retained work and reports
unread failures. A real operation-task panic blocks live owner reuse and leaves
effects unknown. Custom `ModelService` implementations must provide pure
`validate_controls` to support explicit reasoning; the default refuses it.

## Custom tools and managed delegation

For Rust embedders, `CodingApplication::new(host, tools)` adds an optional custom
`CodingToolSource` to the coding tools; a same-name registration replaces that
built-in. Restricted applications instead supply a complete `Application` recipe.
A source publishes `ToolRegistration` values that bind a `ToolDefinition` to a
`ToolExecutor`. Refresh publishes new registrations; an issued request retains
its original executor, including an MCP connection and remote tool name. This
freezes local dispatch identity, not the remote server's implementation.

Executors receive a `ToolInvocation`: the original call, stop token and optional
`ToolOrigin`. Engine-issued origins identify the logical Session, Turn and
committed assistant/tool occurrence; Code Mode children also carry their committed
child index. Reused provider call IDs and model arguments do not supply this
identity. Fields are read-only. Core does not inject provenance into guest or
MCP arguments; applications control what their tool outputs disclose. `ToolCatalog::execute_untracked` and
`ToolInvocation::untracked` explicitly omit admission and origin. Provenance is
not a live lease, capability grant, replay permission or evidence of settlement.

### Delegation context and foreground custody

Rust applications can prepare a child with Host's
`SessionDelegation` and request it through `ToolInvocation::delegate`. Its
private live admission port is separate from the historical origin. The complete
child application, authority assertion and environment are explicit. Choose
`DelegationContext::Fresh` or `Fork { point }`, with `ForkPoint::BeforeTurn(n)` or
`ForkPoint::AfterTurn(n)` selecting the actual parent's committed prefix. An unfinished Turn is not an after-Turn
boundary; an invalid choice is refused before intent or child files.

Forking copies historical conversation/context facts, including instructions,
tool output and opaque provider material. Applications must authorize that
disclosure; a fork is not secret scrubbing or isolation. Current instructions,
model/reasoning, callable tools, cwd and credentials still come from the explicit
child recipe, not the copied configuration. Matching deferred definitions in that
recipe can regain exposure from copied context; parent executors are never added.
Entry coordinates and complete compaction cuts are preserved. The new child link
and explicit preferences append after the prefix in the header's initialization
transaction; inherited delivery keys, continuation grants and child ownership
are not transferred.

Core records a stable child identity and relationship before materialization.
Tasks default to foreground: parent stop propagates, and supervision joins shared
control and started work before parent settlement—even if the tool abandons its
wait. Exact repeated specifications reconcile; different work, ownership mode or
owner epoch at that invocation conflicts. `SessionView::delegations()` and
`parent_delegation()` expose typed relationships without parsing tool prose.

### Background ownership transfer

For background work, retain a Host `BackgroundOwner` and choose its live
destination: `recipe.prepare(input)?.in_background(owner.destination())`. The
coding application owns one owner for its lifetime, shared across runtime reload
and binding replacement; a child recipe does not capture it. Core reconciles
exact repeats first, then reserves Host custody before admitting new work. Even a
full or closed owner permits reconciliation of an existing transfer. The
destination installs a retained operation and Core commits
`DelegationTransferred` before the parent receives its transfer receipt. That
receipt is not child settlement or liveness. Cancellation observed at transfer
admission refuses cold work; a racing cancellation may lose to that admission.
After transfer, parent cancellation and parent-Session shutdown do not stop the
child. Explicit `cancel` stops one child; application shutdown closes admission,
stops its children and joins their full Host controls. Drop requests stop; finish
`join_shutdown().await` before destroying the runtime, which returns the retained
infrastructure error. Cancelled maintenance/shutdown waits retain tasks and
results. `maintain()` joins without consuming; `wait(child)` returns one joined
report without acknowledging it; `child()`/`jobs()` inspect retained status;
only `take(child)` releases a joined result's capacity. Capacity bounds
reservations, running jobs and unread results together (1–64 slots), not history,
encoded bytes or RSS. Changes are published through a revision plus notification;
clients still reconcile snapshots rather than treating notification as delivery.
Snapshots are privileged custody views, not an authenticated remote service or
atomic Core/Host snapshots.

A foreground child's actual original answer is retained with its joined
relationship only after full Host/Core joining and a completed requested Turn.
The Host callback returns that observed text; Core does not reopen the child
database or reconstruct a report. Execution, cleanup or storage failure clears
the answer rather than presenting it as success. A background transfer
acknowledgement is not a result; use wait/status and the same retained outcome.
While a transferred job retains its parent writer, an application rebinding the
same Session reuses that retained handle rather than opening a competing writer.
Unread results do not pin the writer after settlement.

Relationship storage failure blocks dependent execution before release, including
a Code Mode guest that catches the error. Later infrastructure failure or panic
closes the destination owner and requests its other children's stop, independently
of output readers. It cannot retroactively change an acknowledged parent result.
Joined ordinary child failures remain available in receipts. Panic and process
loss do not prove effects stopped.

After owner loss, inspection is passive. The next explicit new-work admission,
direct shell or authorized parent-Turn continuation closes orphan relationships
as unknown before dependent work—even if the original parent Turn already ended.
A surviving transfer task retains the parent writer lease and is not closed by
new parent work. Copies retain foreign history but acquire no child ownership.
Recovery neither restarts the old child nor rewrites its missing observations;
an owner UUID is not authority to adopt or resume it. New model-issued delegation
is new work, not an exactly-once effect guarantee. Live-model delegation
qualification remains open.
