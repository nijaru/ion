# Ion architecture

This document owns accepted engine and application contracts, including targets
not yet implemented. [README.md](README.md) describes delivered behavior and
qualification limits. Current source establishes implementation. Ion is unreleased
v0: replace obsolete APIs, development formats and owners rather than retaining
parallel implementations or compatibility facades.

## Product

Ion is a composable Rust agent engine. A complete TUI and CLI coding agent is its
first application and qualification workload, not the boundary of the engine.
Embedded applications, bots, durable services, phone integrations and multi-agent
coordinators must be able to use the same execution and context machinery without
reimplementing the model/tool loop or reaching into executable-private state.

Ion owns agent execution, durable execution facts, shared control and managed
Session delegation. Applications may own deployment, transport, authentication,
business data, delivery queues, scheduling and swarm strategy. No mandatory daemon,
generic workflow language or Ion-owned coordination service follows from this.
Leaving infrastructure external must not leave callers to reconstruct acceptance,
execution identity, parentage or settlement themselves.

Pi and fx are primary implementation references for harness design and workflows;
fx also guides restrained presentation. Codex is a secondary Rust reference, and
Claude Code's documented contracts provide secondary workflow/SDK evidence. Inspect
relevant source; distinguish established behavior, experiments and vendor claims.
Adapt semantics to Rust, not entire runtimes or feature inventories.

### Reference-derived design

[Pi](https://github.com/earendil-works/pi) and
[fx](https://github.com/vercel-labs/fx) are living upstream baselines, not pinned
parity targets. Use their current relevant implementation when making a design
decision. Historical revisions identify evidence, not the version Ion must match.
Separate shipped native workflows, examples/extensions and experimental SDKs;
one surface does not inherit another's guarantees.

Comparable workflow completeness, reliability and polish are the target, not
matching language APIs, crate diagrams or feature counts. Ion's coding workflow
must support inspection, guarded changes, native verification and reporting;
editable input during work; stop/reopen with honest effect uncertainty; useful
long-session context and evidence retrieval; trusted customization; coherent
model setup; and responsive, inspectable terminal interaction. The detailed
contracts below govern those capabilities across clients.

Use typed states/errors, explicit owned work, narrow replaceable boundaries and
asynchronous stop/join. Persist data, not tasks, trait objects or credentials.
Keep application policy out of Core. Introduce abstractions for real variation,
not merely to resemble a reference SDK. Volatile input custody, committed admission
and restart-safe queued custody are distinct promises, explicit at acknowledgement.
External deployment must not introduce a second execution ledger or agent loop.

### Capability decisions still open

The contracts below do not settle every reference feature. These capabilities
must be considered explicitly rather than hidden under broad claims of parity:

- **Coding context:** broader search/provider coverage and richer document retrieval; login-shell startup
  state capture beyond explicit command setup; non-image document/media support; summary-quality
  aids beyond original-evidence retrieval.
- **Continuity:** optional branch-recall summaries and labels; richer export/share
  formats; history and artifact retention policy. Basic context navigation and
  independent conversation copies have distinct contracts below.
- **Customization:** packaged user-installable contributions, arbitrary command
  handlers, lifecycle/policy consumers, external renderers, themes and configurable
  keybindings. Named executable commands and Rust/MCP tools do not deliver all of
  these powers.
- **Model access and accounting:** subscription/OAuth workflows, additional
  provider protocols, virtual-model routing, user-facing spending/usage reports
  and budgets. Context-token calibration is not a spending budget.
- **Clients and distribution:** ACP/editor adapters, non-Rust SDK bindings,
  browser/WASM hosts, platform support beyond currently qualified environments,
  installation/update workflows and user-facing health/trace diagnostics.

These are unresolved product/delivery decisions, not promises to copy every
reference feature or permanent exclusions from V1. A selected capability needs
an intended workflow, an owner, authority/failure semantics and a real consumer
before implementation. Optional approval policy and durable-hosting boundaries
already have accepted contracts below; their unfinished implementation is not
an unresolved decision about whether those boundaries exist. Delivery order and
historical comparison evidence do not belong in this architecture document.

## Engine concepts and owners

- **Session:** one ordered committed history and its derived conversation,
  context and execution state. Stable logical identity is separate from a file
  path, display name, provider affinity and process-local handle.
- **Turn and active operation:** a Turn is the logical work from accepted input
  through model/tool steps to settlement. Its durable identity is distinct from
  the live operation task. One exclusive operation runs per Session; different
  Sessions may run concurrently. Coding's direct shell and manual compaction are
  operations, not additional agent loops.
- **Prepared request:** one model selection, bounded context and immutable
  catalogue binding declarations to executable routes and captured authority.
- **Agent configuration:** caller-selected model services, instructions,
  capabilities and execution environment. Coding tools, project discovery and
  local workspace behavior are an application configuration, not universal defaults.
- **Ownership:** live work has a controlling owner independent of observers.
  Delegation records the relationship between independently executing Sessions;
  it does not create another worker engine.

```text
TUI / CLI / RPC / embedded application
          │ commands       ↑ snapshots, facts and activity
Host: shared Session control ← complete application recipe
          │ admission, execution, stop/join
Core: Session + context projection + one Turn engine
          │ frozen model/tool contracts
Host: provider adapters, trusted tools and execution environments
```

This is control flow, not a cyclic crate dependency. `ion-ai` defines neutral
model/content contracts; Core depends on contracts rather than concrete Host
implementations. Host assembles the implementations. App and Terminal do not
provide alternate engines.

| State or responsibility | Authoritative owner |
| --- | --- |
| Committed conversation, accepted inputs, execution relationships and observations | Core Session; derived indexes and projections are rebuildable. |
| Request context, frozen catalogue, generation and effect ordering | Core Turn engine; providers/tools implement captured contracts. |
| Live operation task, stop and joined outcome | Core operation owner, independent of waits and observers. |
| Submitted but unaccepted inputs, preparation, follow-ups and replacement | Host shared control; clients retain only unsent drafts and presentation state. |
| Resources, credentials, model/tool environment and application policy | Host application recipe and prepared binding; active work retains its captured binding. |
| Live delegated agents and native programs | Host owners; Core records delegation relationships and tool observations, not live OS handles. |
| Semantic transcript/activity projection | Core facts and live previews, rendered by App without turning previews into facts. |
| Editor, pickers, local diagnostics and protocol correlation | App/client; local presentation is not model history. |
| Physical paint, input modes and terminal restoration | Terminal publication/input owners. |

Input custody is a transfer protocol, not a requirement to store every stage in
one queue. Shared control retains originals until Core confirms committed
acceptance; Core may hold transferred steering until an atomic assistant/input commit.
Only the current custodian can dispatch or return an input. An application
coordinator may compose runs, not implement a second model → tools → model loop.

## Application configuration and authority

High-level shared control must accept a complete application configuration, not
require lower-level integration to escape coding defaults. A restricted bot must
be able to select only its own capabilities and instructions without inheriting
native shell/file tools, project rules or operator-home resources. Model and tool
implementations remain replaceable through the same lifecycle API.

Native command configuration belongs to the Host binding, not process-global
mutable state or model arguments. The coding preset captures inherited variables
and Bash/sh selection, with optional operator-configured Bash-compatible setup;
it does not implicitly rerun login startup. Libraries may provide a complete
environment without user configuration. Exec, direct shell, named commands,
managed pipe/PTY and coding children retain the same captured policy; replacing a
binding does not modify queued work or running programs. Setup is part of admitted
execution, including its effects, cancellation and failure. Generic argv/MCP
launchers retain their separately configured startup/credential contracts.
Environment values and setup are not recovery data; the trusted Host reconstructs
and attests environment/principal/grants under the existing authority scope.

Context inheritance, workspace/environment binding and callable authority are
independent choices. A Session need not require a local coding directory. A child
may inherit selected conversation context while using another workspace. Explicit
configuration and prepare-before-publish replacement must preserve these choices
across model changes, reload, reopen and Session creation.

Applications authenticate callers and authorize control/inspection. Host tools
capture trusted principal, environment and grants; model-provided arguments cannot
grant authority. Execution receives trusted Session/Turn/tool-occurrence identity
when needed for delegation, idempotency or audit, not a parent inferred from text
or a reusable provider call ID. Persist declarative identity/scope needed for
recovery, not live credentials or executors; unavailable or changed authority must
fail or be explicitly reconciled, never silently rebound by name.

Capability selection is not OS isolation. The coding configuration deliberately
uses the host user's permissions, not an implicit sandbox, VM or rollback facility.
Untrusted multi-tenant/device integrations need restricted tools and appropriate
execution isolation. Approval and consent are application policy, not an implicit
engine UI; a future approval boundary must decide before effect dispatch and
cannot retroactively stop started work. V1 does not claim OS sandboxing. OAuth is
an optional access workflow, not a first-use gate.

## Agent loop and effects

The principal lifecycle is:

1. Assemble and validate the application's resources, model and executable tools
   before publishing a binding. Prepare client input without admitting effects.
2. Commit accepted input and Turn identity. Project bounded context and freeze
   the exact model/tool request before issuance.
3. Generate, commit the assistant response and call intents, execute admitted
   effects, and commit their observations before dependent consumption.
4. Continue through that same loop or atomically commit the final answer and
   Turn end. Join owned work before reporting operation settlement.
5. Project facts and live previews to observers; presentation failure cannot
   retroactively change admission or observed effects.

Direct batches execute and commit in call order by default. Allowing a provider
to return several calls is not permission to overlap their effects. Opted-in
composition already admits bounded concurrent tool children. Do not infer safe
parallel execution or replay from a read-only label, schema or remote tool name.

Any direct-batch overlap must be an explicit application policy over frozen,
executor-declared eligibility, with ordered execution as the fallback for the whole
batch if any call requires it. Commit completed observations promptly by occurrence,
join all started work before another model request, and preserve stop, steering and
cache-warming custody. Retain the ordered default until a representative comparison
justifies the additional in-flight uncertainty and lifecycle complexity. Pi's
parallel default is a reference trade-off, not measured evidence for Ion. There is
no fixed production step-count cap masquerading as task completion.

- Commit calls and resolved semantic activity before dispatch; commit observations
  before dependent model or guest consumption.
- Commit final answer and Turn end together. Assistant plus admitted steering is
  also atomic; failed commits leave unaccepted input recoverable.
- Admission, dispatch, observed outcome and settlement are distinct. Publish
  authoritative events after commit. Observed, not dispatched and unknown are
  different outcomes, not interchangeable error strings.
- Reject malformed/truncated calls before effects. Identifiable output-truncated
  calls may commit with synthetic not-dispatched results; otherwise do not invent
  call identity. A completed empty response is not successful work.
- Stop closes dispatch, requests cancellation and joins started work. Rendering,
  observation and connection failures cannot drop execution custody. Cancellation
  is not rollback, a hard deadline or proof of descendant/remote quiescence.
  Storage failure blocks dependent work; uncertainty grants no replay authority.

## Session, durable admission and recovery

Use one typed append-only SQLite history, an exclusive writer lease and atomic
related-fact batches. Validate candidate state before writing and publish derived
state after commit. Required durable submission, ownership and recovery facts
belong with execution truth, not a second mutable execution ledger. Business
queues/state may remain external; another storage backend needs a concrete consumer.

Distinguish these promises:

- **Conversation continuation:** reopen committed context and submit new work.
- **In-process retention:** cancelled waits do not abandon tasks or outcomes.
- **Live attachment:** observe work retained by a surviving owner.
- **Durable submission:** recover an acknowledged request and its acceptance after
  restart without submitting it twice.
- **Logical-Turn continuation:** explicitly recover authority to continue the same
  interrupted work through the existing engine.

A local queue acknowledgement is not durable acceptance. Durable submission must
bind a caller-scoped request identity to its committed admission and outcome.
Repeating that identity returns the same admission/status rather than starting
another Turn; conflicting content is an error. If queued custody is promised
across restart, persist it before acknowledging that promise. An external delivery
queue must be able to reconcile acceptance without guessing from prompt text or
maintaining competing execution truth. Admission deduplication does not make
external tool effects exactly-once.

Open and inspect passively. A committed call without an observation is unknown,
even if it might never have begun. Conservative recovery atomically closes
interrupted work before new authority. Logical-Turn continuation requires an
explicit recovery policy and reconstructed, validated authority; it does not
restore an arbitrary future, rerun a whole effectful Turn, or replay unknown calls.
Tool-specific reconciliation/idempotency may resolve uncertainty. Process loss
and task panic do not prove effects stopped. Durable and foreground operation
must use the same loop and effect ordering, not separate execution implementations.

Coding's direct shell commits command and context-sharing choice before dispatch,
then the actual host observation. Missing results remain unknown. `!` shares
command/outcome with model context; `!!` excludes both, including interruptions.
Clone/fork creates independent conversation/context from a valid committed prefix
with fresh provider affinity. It neither snapshots nor rolls back a workspace,
nor establishes delegation or security isolation. An unfinished Turn is not a
settled fork boundary. Empty Sessions must not displace useful recent conversations.

Saved-Session selection is passive: expose identity, name/input preview, recorded
model, Turn count, activity time and the current selection before switching through
idle shared control. Discovery must not monopolize terminal input; dismissal and
replacement retain cancellation/join custody and cannot publish stale results.
Selection does not submit a prompt or repair history. Both before/after fork
pickers use committed Turn facts; before restores editable input without sending,
and after requires a recorded end. Do not infer Session ancestry from matching
content or timestamps.

Context navigation selects a parent-linked path within the same logical Session;
execution facts, Turn IDs, tool occurrences, deliveries and child ownership remain
in one append-only ledger. Selection is durable without another prompt. Core owns
valid complete call/result cuts and a Session/revision/head token; shared control
holds the engine gate through resource/model preflight, commit and publication.
A stale preview or definite precommit failure leaves selection unchanged; a
postcommit publication failure requires reload, not an assumed rollback. Interrupted
facts close conservatively in the same transaction, never by replaying effects.
User-message selection restores editable typed input without submission; assistant
and settled tool-group points continue after their observations. Library, RPC and
terminal callers share this owner. Background outcomes retain their initiating
occurrence, not the currently displayed path. Navigation does not restore files,
transfer work, widen grants or generate summaries. It clears request calibration
and old opaque continuation; historical context is not current executor authority.
Human conversation/export and model context follow the selected path; inspection
and evidence retrieval retain original global coordinates and all alternatives.

## Managed delegation and coordination

A managed subagent uses an ordinary Session and explicit owned runs. Record each
run's stable identity separately from its reusable child conversation, initiating
occurrence, target/context, capability/environment scope, ownership and outcome.
Exact redelivery at an invocation reconciles the recorded run; a new request has
a new run and request key even when it reuses a Session. Parentage and every run's
outcome must be available without parsing prose or overwriting previous reports.

- **Foreground:** child work participates in parent settlement. Stop propagates;
  started child work joins before parent settlement. Cancelling a wait does not
  abandon the child.
- **Background:** responsibility transfers explicitly to a longer-lived owner
  before the parent releases it. Detaching a handle is not a transfer. Parent
  cancellation and service shutdown have distinct, documented scopes.

A bounded foreground join reserves a live background destination before intent.
Its record names an intended owner, not an already-committed transfer. The budget
starts at live Core admission, includes child materialization/control setup, and
excludes synchronous recipe preflight. Prefer actual full completion to a ready
join expiry. Otherwise commit transfer before releasing the parent, moving the
same pinned callback, run, request, stop scope and steering grant. Do not restart
work, reload authority or refresh an execution deadline. Until transfer, parent
stop cancels and fully joins even beyond the join budget. An abandoned result
waiter retains foreground joining rather than silently transferring. A stop or
waiter closure observed before transfer prevents it; one racing commit may lose.
Refused/failed installation must leave started execution with the gateway to stop
and join, preserving infrastructure failure. Completion without transfer releases
the reservation and retains no background report. A transferred result occupies
capacity until explicit acknowledgement.

Host supervises children using the same engine and shared control. Recovery keeps
unknown child effects explicit and reconciles the relationship before redispatch.
Fresh context and an explicit fork are different. Neither automatically grants
parent secrets; confidentiality requires actual capability/environment isolation.
Code Mode's nested tool calls are not child-agent Sessions.

Explicit resume starts a new run in the same settled conversation, not a fork,
replay or adoption of an earlier owner. A fresh authenticated application captures
runtime, resources, route and grants before admission; saved paths, UUIDs, settings
and ancestry supply no authority. Resolve the expected Session identity, acquire
its exclusive writer and recheck identity, original cwd and settlement before
committing parent intent. Reject the active parent as its own child, live writers,
active Turns, pending shells and unresolved owned background work. This entrypoint
does not implement unfinished-child recovery. Keep the writer through actual
control/Core/native cleanup, so a later run cannot overlap that cleanup.

Append the new managed request binding and applied model/reasoning without
rewriting prior facts, request keys or selected context alternatives. Several
runs may share one Session; settlements validate both its UUID and the requested
run's key. An older completed receipt cannot satisfy a newer request. Old runs
retain independent waiters, stop/control scopes and unread reports, each counting
against custody until acknowledged. Acknowledgement releases report capacity,
not the saved conversation. Child bindings are historical provenance, not grants.

Execution budgets belong to the shared Host child lifecycle, not a tool's
borrowed result wait or a client loop. `DelegationSpec.execution_timeout_ms`
records the positive optional policy; `SessionDelegation` arms one monotonic
deadline at linked request submission after preflight/materialization. Control
progress and parent wait budgets never refresh it. Already-observed settlement
ends execution timing; when stop and expiry are both ready, stop takes precedence.
Expiry requests ordinary stop and fully joins control and native cleanup, without
imposing a second cleanup deadline or discarding started work.

A joined relationship retains typed `execution_timed_out` provenance separately
from execution/infrastructure errors. Trusted callbacks return
`DelegationCompletion`, enforcing their specified budget and attesting actual
expiry; Core never infers it from strings or elapsed wall time after owner loss.
Joined outcomes also retain optional Host-observed `elapsed_ms`, from linked
submission through observed stop or settlement, excluding subsequent cleanup.
Unknown timing is null, not zero or elapsed time inferred after owner loss.
A failed run can retain `partial_output`: the latest observed visible assistant
response, bounded to a 50 KiB UTF-8 prefix with explicit truncation/gap flags.
This is provisional child-authored data, not a successful answer, durable assistant
commit, verified effect or safe-retry evidence. Thinking, tool arguments/results
and images are excluded. Never join deltas across missing progress or retry
boundaries; a reliable settlement can replace lost progress. Completion-only
services must expose their visible text even when incomplete output cannot commit.
Success has its independently validated original answer and no duplicate partial
report. Core validates bounded failed observations and retains them with timing
and timeout/error facts through inspection, wait and acknowledgement.

Host also exposes the same bounded observation while the child runs through
`PreparedDelegation.progress` and retained owner snapshots. It carries actual
child/request coordinates, observed UTC last-activity time, content-free activity,
a bounded top-level tool name and explicitly `provisional_output`. No thinking
text, tool arguments/results or images enter this projection. Reads compute live
elapsed time but never refresh activity or a deadline; observed stop/settlement
freezes elapsed before cleanup. Silence is not a stall verdict or heartbeat.
Owner invalidation signals observation changes, not joining. This read-only handle
holds no Session writer or execution/cancellation grant, and survives acknowledgement
only as ephemeral observation data, not recovery authority. Core's joined outcome
remains the durable result; no live progress is written as relationship facts.
Terminal `/tasks` consumes content-free metadata without copying child output;
`/tasks status` explicitly requests the bounded provisional preview. Both render
through the same literal, control-safe inline/fullscreen notice path and remain
terminal-local output, never model input. Stopping is still unjoined, errors
precede previews, and result/acknowledgement still require actual joining.

Timeout is a failed relationship with no answer, not a rewrite of the child's
ordinary Turn end. Infrastructure failures remain fatal even alongside timeout.
Lost custody remains `Unknown`, not timed out, rolled back or safe to replay.

Live child guidance uses the same Host input custody as root steering. A prepared
Host recipe returns its cold task and an ephemeral control grant; background
transfer retains the matching grant with the actual run and verifies both child
Session and run request identity before admission. A grant is fenced to one model operation and stop scope,
not a Session UUID or a serialized lease. It cannot follow rebinding or start a
later Turn. There is no second steering queue, agent loop or direct Core-inbox
bypass. Literal inputs share existing admission capacity, byte reservations and
FIFO transfer; ID order is actual admission order even with concurrent producers.

Queued acknowledgement is not committed steering. Receipt projections track
atomic Core commitment, original input returned on stop/end, or unknown custody
when control is lost before joining. Core consumes inbox entries only after
persistence; joined Host reconciliation preserves consumed input's acceptance even
if a post-commit observer failed. Never classify it as unaccepted/replayable.
Commitment is not proof of a later model request or understanding. Cancelling
readiness leaves the child alone; abandoning a waiter cannot recall an input
already admitted. Live request keys reconcile exact input only within the retained
grant, not after owner loss. Receipt changes invalidate the supervising owner
without a new task or reader-owned wakeup; pending intent/projections are not a
second durable ledger. Committed child input remains in the child Session.

A swarm coordinator may choose roles, topology, message routing, budgets and
scheduling. Ion supplies execution identity, authority, observable relationships,
stop/join and outcomes. Concurrent agents sharing files need conflict policy;
cooperating-writer locks alone do not resolve incompatible edits. Worktrees,
containers or read-only roles are strategies, not a mandatory engine architecture.

### Coding delegation policy

Managed delegation is a normal capability of the main coding preset, not a
feature gate imposed on the reusable engine. Restricted applications still choose
their own complete capabilities. Child coding roles do not automatically enable
further delegation.

A coding child defaults to the initiating request's captured model route and
reasoning. `start` may explicitly select an exact `provider/model` from the known
catalog or configured routes and independently override reasoning. Inherited
reasoning remains inherited even with another model; unsupported controls, unknown
routes and missing required access fail preflight before child admission. There
is no fallback or additional child-route allowlist. Same-identity overrides retain
the captured route; a different route is resolved and captured before admission.
Overrides do not change the parent binding or operator configuration. Receipts
report the actual child model and reasoning. Coding `start` additionally sets a
child execution budget, default 30 minutes with `execution_timeout_ms` accepting
1–7200000. This applies to foreground and background children independently of
parent waits; general Host recipes choose their own positive optional budget.

The workspace defaults to the parent's; explicit `cwd` selects an absolute path
or one relative to the parent. Load project/home resources for that workspace,
retaining the captured command environment and selected native/external tools.
Model arguments cannot supply endpoints, credentials, tool grants or arbitrary
authority. General Host recipes configure target, model, environment and authority
independently. Workspace selection and exclusive Session writers are not filesystem
isolation or all-descendant quiescence.

Fresh context and bounded foreground joining are defaults. Coding foreground
`start` uses `join_timeout_ms`, default 60000 with 1–600000 milliseconds allowed.
It reserves application capacity before starting. Before expiry it returns the
fully joined result without transfer; otherwise it returns the same running `id`
with `join_expired: true` after committed transfer. This is not execution timeout
or success; use wait/status rather than starting replacement work. Explicit
background mode rejects a join budget and transfers immediately. Generic Host
recipes retain unbounded foreground by default or select their own duration.
An explicit fork selects an eligible committed prefix. `start(resume=path)` instead reuses a settled Session
under a new run, requires its original cwd, and is mutually exclusive with context
copying. Both locator and cwd accept at most 4096 UTF-8 bytes. As with a fresh
start, omitted route/reasoning inherit the current request's captured configuration,
not the saved child's preferences; explicit overrides receive the same preflight.
Background execution transfers to the coding application's managed owner. The delegation destination is shared across runtime
reload and binding replacement, not captured by child recipes. Parent cancellation,
explicit child cancellation and application shutdown have distinct scopes.
Parent Session shutdown does not stop transferred background children.
Application shutdown stops and joins them. A one-shot headless host retains
ownership through normal completion; interruption, RPC EOF and terminal quit
stop and join it before potentially blocking final publication. No daemon is
implied.

Model-facing coding actions are start/list/status/wait/steer/cancel/acknowledge. A
background start receipt means committed responsibility transfer, not completion.
Receipts expose `id` as the run handle and `child_session` as the conversation
UUID. Status/steer/cancel/acknowledge take `id`; wait takes `ids`, never conversation
identity. Wait and status return retained typed outcomes without consuming them.
Wait selects 1–64 unique owned runs and returns all selected statuses, in requested
order, when any actual operation joins or one shared absolute wait budget expires.
Validate the entire selection before returning even a ready report. The coding
wait budget defaults to five minutes and accepts 1–600000 milliseconds; the shared
Host API accepts a caller-supplied duration, including zero for retained-join
polling. Unrelated invalidations do not refresh it. `wait_expired` means none of
the selected operations had joined when sampled, not child execution timeout or
cancellation. A joined failure also satisfies wait-any and remains a failure.

Concurrent waiters have independent notification subscriptions; cancelling or
expiring one neither strands another nor relinquishes operation custody. Waiting
does not stop children, change execution deadlines or acknowledge results.
Acknowledgement releases only joined capacity. A child's original visible answer
is retained only when its requested Turn completed and full Host/Core joining
succeeded. Failure clears it; it is never reconstructed by reopening storage.
Notifications are invalidations, not result delivery or completion proof.
Coding `steer(id, message)` targets a live owned background run with nonblank
literal guidance of at most 64 KiB UTF-8. It neither changes captured model,
resources/instructions or authority nor resets the execution deadline. Stopped,
settled, unknown and acknowledged run IDs reject new guidance; steering never
implicitly resumes a conversation. The general Host control grant is also available
to a foreground child's application caller.

Do not fabricate user instructions or successful answers to deliver
notifications. Automatic root continuation must not silently select or adopt a
managed child; explicit child selection remains available. Live-model adherence,
service-scale delegation and native GUI visual acceptance remain qualification,
not reasons to narrow the designed coding workflow.

## Managed native programs

A managed program is a Host-owned OS process, not an agent Session or another
Core loop. Its model-facing start/control observations use normal committed tool
intents and results. A start receipt observes creation and live custody, not
command completion. Missing observations remain unknown; reopening a conversation
never restarts a command or reconstructs a live grant from a saved PID.

The coding application retains one live supervisor across Turns and binding
changes. Programs are scoped to their initiating Core Session and captured working
directory: a guessed handle, another Session or a conversation copy cannot control
them. Application shutdown closes admission, requests stop for both managed
programs and delegated agents, and joins both before releasing their environment.
Dropping an observer or stopping a Turn does not terminate an already-started
program. A start cancelled before its receipt stops and joins its admitted process;
subsequent wait cancellation only ends observation. Explicit close requests stop
and joins. Cancelling that join reports an incomplete close, not successful
settlement or rollback; stop remains requested and later wait/close joins the same
retained work.
This is in-process custody, not a daemon, OS sandbox or all-descendant containment.
A coding child job stops, joins and releases its own program scope before its
answer is released; it cannot transfer that grant by returning a handle to its
parent. The retained application supervisor still guarantees cleanup if a child
fails. Keeping a parent service alive requires parent-owned creation, not a leaked
child resource.

Separate process state, input ownership, raw capture and presentation. Pipe-backed
programs support background servers and line-oriented stdin, not terminal control
sequences or full-screen interaction. PTY programs require controlling-terminal,
resize and derived screen semantics; human takeover additionally requires exclusive
write custody. These extend the same process owner rather than introduce another
runtime. Shell startup and environment capture are independent from process lifetime.

PTY start explicitly selects a terminal backend with initial dimensions; ordinary
pipe execution remains available. The child gets a fresh OS session, controlling
terminal and foreground process group. Terminal stdout/stderr merge into one raw
stream, not two fictitious pipes. PTY EOF is terminal input interpreted by its line
discipline, not a half-close of the master. Closing and waiting retain the same
custody rules as pipe programs, including startup cancellation and child-scope cleanup.

A bounded live terminal parser consumes every output byte before raw eviction.
Screen reads are paged projections with a source-byte cursor, revision, dimensions,
visible cursor and input-mode metadata; they are not replayed tool output or durable
terminal resurrection. Resize changes the owned terminal and its projection together;
failed OS resize must not publish successful dimensions. Required terminal protocol
replies are bounded, serialized with input and cannot access clipboard, host display
or arbitrary services. Unsupported terminal behavior is not silently advertised as
complete emulator support. Projection-only failures, including oversized control
strings, make the screen explicitly unavailable while raw capture and the program
continue. Required-reply overload is instead a protocol supervision failure: stop
and join the program, retain raw capture and report the failure, never silently
drop required replies. Neither failure relinquishes process custody. Human input
requires a real client owning an exclusive input lease; model-only terminal
control does not establish a human-attachment workflow.

Bound live slots, retained results, pending input and raw capture. Reads use explicit
byte cursors and report evicted gaps, decoding loss and incomplete capture. Exit
status and pipe EOF are distinct observations; a known direct-child exit must not
silently certify complete output. Waits retain outcomes, close retains final evidence,
and explicit acknowledgement releases joined capacity. Unknown or failed supervision
must not become a fabricated successful exit. Raw bytes inform a screen projection;
rendered text and returned windows are not a second execution log.

## Capabilities and trust

A registration binds definition, semantic metadata and executor. Freeze them as
one request catalogue. MCP bindings capture connection and original remote name;
unavailable connections fail instead of redirecting or replaying calls. Local
capture cannot freeze a remote server's implementation.

Callable authority and model exposure differ. Direct tools are declared; deferred
tools are callable but omitted from the declared loadout until discovery activates
them for a later request, never the issuing request. Hidden capabilities are not
model-reachable and their MCP resources are omitted from resource tools. Restore
activation only while its definition remains identical. Exposure changes affect
future requests, never an issued request. Applications may narrow the
model-visible loadout with include/exclude patterns. An absent inclusion imposes
no include filter; an explicit empty inclusion selects no tools. Excluded
capabilities are rejected before model dispatch and omitted from discovery and composition. An
intrinsic discovery tool is available only when the selected loadout allows it.
Selection is application configuration for its managed children, not a session
guarantee, authority grant or sandbox. Tool visibility is not a permission system
or sandbox.

Host validates untrusted arguments/content. Native UTF-8 reads return complete
lines by default; explicit byte pages use actual character-boundary continuations
without clipping or skipping source bytes. The same bounded reader retrieves
raw saved observations even when a physical line exceeds the page allowance.
Native exact edits require the
complete-file digest returned by read and validate disjoint replacements against
one original snapshot; write replacements require that digest unless the caller
explicitly opts into unconditional replacement. Creation remains unguarded.
Edits preserve unmatched bytes/BOM/line endings and respect effective write
permissions. Bound inline capture and identify omission, decoding loss and
incomplete EOF. Full artifacts have a separate disk
lifecycle, not an aggregate quota or retention guarantee. Images have one bounded
normalization owner across user/native/MCP inputs; unsupported content is explicit.

The coding application includes selected-URL HTTP(S) retrieval in direct and
composed loadouts. It uses the execution host's network, including local/internal
destinations, without importing browser state, provider credentials or ambient
proxies. Neutral applications gain no network tool implicitly. Bound redirects,
network acquisition, conversion and output; preserve requested/final source URLs,
completed-body digests and explicit failures. Retrieved content remains untrusted
data, never instructions or grants. Conversion and snapshot workers remain owned
through stop/join. Omitted converted text may have a private temporary snapshot,
not a durable reference or retention lease. GET, a read-only label and equal tool
declarations grant no replay authority. Other document formats remain separate
capability decisions.

Coding search prefers the active model/provider's supported search contract, with
its resolved profile endpoint, model and credential resolver. Operator-configured
Exa, DeepSeek and Brave routes are the preferred direct fallback tier; their order
is a configurable tie-break. Capture search policy with the application binding;
managed children retain that policy but resolve native search against their own
selected profile. User settings have one bounded snapshot, not competing readers.
General applications opt into search explicitly. Model arguments may select an
admitted backend but never supply endpoints, credentials or execution models.
Send only the explicit query, not implicit conversation/files. Bound acquisition
and results; retain structured source URLs, response digests and known reported
usage, including on incomplete native responses. Never manufacture source evidence
from answer prose. Select fallbacks before dispatch only: a timeout, HTTP failure
or malformed response does not authorize another potentially billable request.
Native search may incur both model and search charges; a read-only label is not a
free-call promise, spending budget or replay grant.

The coding application includes native `ls`, `find` and `grep` for repository
exploration without shell authority. Explicit selection and the read-only loadout
apply to direct and composed calls alike. Paths resolve against the captured
working directory; absolute paths are allowed with host permissions, not confined
to a repository. `ls` exposes immediate entries including hidden names; recursive
file discovery and text search respect ignore rules, include non-ignored hidden
files, exclude `.git` traversal and do not follow discovered symlinks. An explicit
root may resolve a symlink as native reads do. Glob matching, regex versus literal
search and line/byte locations are declared tool contracts, not shell syntax.

Bound inspected entries, text bytes, pattern compilation, output and diagnostics.
Search results identify their root and source locations so `read` can retrieve
actual evidence. Report omissions, traversal/read errors and budget exhaustion;
an empty partial search does not establish absence. Oversized, non-text and
unrepresentable paths must not become plausible-looking empty or renamed files.
Results describe a live scan, not an atomic filesystem snapshot. After a limit,
callers refine the root/filter; no cursor may falsely promise a stable restart.
Filesystem workers remain owned through cancellation and settlement, including
uninterruptible native syscalls. These tools grant neither mutation nor automatic
replay authority and do not replace the existing command workflow for builds and
verification.

The executable selects one user home: `ION_HOME`, defaulting to `~/.ion`.
Configuration, credentials, resources and Session subdirectories share that root;
XDG locations are not fallback sources or implicitly migrated. Explicit embedding
recipes may still supply separate configuration and state roots. Project `.ion`
remains a distinct layer; a physical alias of the selected user directory is not
loaded again as a project source or assigned project trust.

The coding preset loads user defaults from its configured `AGENTS.md`, then
filesystem ancestors through the working directory. These standing resources are
captured at admission; reload changes future admissions. Project rules apply to
their source directories and descendants, with narrower rules refining broader
defaults and linked-worktree sources shadowing the main checkout where applicable.
Native filesystem targets discover additional physical-directory rules, including
explicit paths outside the working directory; shell text and custom tool arguments
are not guessed. Reads/discovery may return new rules alongside their observations.
Native mutations defer new, changed or removed scoped instructions until the
snapshot is present in the issuing request's tool context. Sibling/hidden Code Mode
results are not model delivery, and stored/compacted facts alone are not an
acknowledgement. No issued request or signed history is rewritten. Instruction-aware
native mutations coordinate within the process, bind their physical target and
recheck rules under the mutation lock. This is not atomic isolation from external
editors, other processes or commands. Explicit applications inherit no such policy
unless they select it.

Resources do not execute repository text or start unconfigured servers. Instructions,
skills/templates and results are lower-trust data. Report failures/omissions with
provenance, not silent rule truncation. Project-scoped MCP configuration is inert
until an explicit trust decision binds the canonical project root to the current
file digest; editing the file invalidates trust. Untrusted entries are not
connected, trusted project entries may override user entries by name, and trust is
not sandboxing. MCP discovery isolates server failures and refreshes at later
request boundaries; possibly effectful invocations are never transparently
retried. MCP shutdown retires every peer before joining connection cleanup;
in-flight requests release read leases without requiring a remote response or
caller cancellation. Concurrent closers join the same cleanup. Unanswered effects
remain unknown. Already observed tool and resource results retain their original
outcome through local conversion. Run image processing, listing materialization
and artifact writes off async workers; calling operations retain and join this
work through cancellation, independently of connection cleanup.

Enforce incoming MCP budgets before framing or decoding can allocate an unbounded
message: at most 64 MiB per stdio JSON line or HTTP JSON/error body. SSE events
retain their transport's stricter 16 MiB default and cannot exceed the aggregate
64 MiB per-message ceiling. Reject overflow rather than truncate a message into a different
observation; do not reconnect and repeat an effectful call. An unanswered call on
a failed transport may already have executed. These are transport budgets, not
model-context or artifact quotas. Tool and resource requests retain a recorded
framing rejection even when connection shutdown joins concurrently.

Conversion preserves the server's observed error/success flag, supported text and
structured data even when another block cannot be delivered. Unsupported media or
invalid images produce indexed conversion notices, not a replacement execution
failure. Retain the original response in a private artifact when conversion omits
content; report artifact failures without inventing the missing evidence. Inline
previews retain the outcome, notices and retrieval references. Validation still
rejects invalid image data from image delivery; preserving a response is not
permission to render or execute unsupported content.

Local MCP stderr is diagnostic data, never direct terminal publication or synthetic
conversation. Capture a private 64 KiB prefix per connection, report omitted bytes
and capture failures, and expose its reference through startup diagnostics and
tool results.
Continue draining after the prefix fills so logging cannot block protocol work.
Own the reader through startup failure and connection shutdown; stop and join it
after bounded final draining, including when descendants retain the pipe. Server
retirement is retained independently of a particular shutdown waiter.

MCP resources are bounded read-only context operations:
listings use frozen server identity, reads use the ordinary image or private-file
path rather than model-visible base64, and an observation grants no effect,
settlement or replay authority.

### Customization delivery and powers

Keep executable customization separate from inert resources. Use compiled Rust
application registrations for in-process implementations and supervised external
processes for user-installable code; MCP is the established external tool/resource
protocol. This avoids a Rust dylib ABI or a second long-lived JavaScript engine.
Code Mode remains invocation-scoped composition, not an extension loader. Named
executable commands are one external contribution with a real shared-control
consumer; they do not establish a general extension-handler protocol, event bus
or workflow language.

- **Data:** instructions, skills and prompt templates remain lower-trust input,
  with provenance and explicit expansion, never executable launch instructions.
- **Model tools:** bind declarations and executors in the frozen catalogue through
  Rust registration or MCP. Existing selection and exposure rules still apply.
- **User commands:** explicit user invocation expands input or requests an existing
  typed shared-control action. External commands receive literal arguments and
  bounded input/output, cancellation and joined supervision. Effects use the
  action's ordinary admission/recording contract and explicit model-context sharing;
  command output is not a fabricated user message or model tool call.
- **Lifecycle hooks:** receive immutable committed facts at named boundaries.
  A pre-dispatch decision hook can allow or veto a specific invocation, not rewrite
  its arguments or observed result. Deliver that seam with an actual policy or
  approval consumer, not unused hook variants. Stop/shutdown retains and joins
  started hook work; observer loss cannot silently abandon it.
- **UI contributions:** produce bounded presentation data through the client owner,
  not raw terminal writes. They cannot change execution, input custody or history;
  failure leaves the underlying observation inspectable.
- **Providers:** explicitly registered Host adapters own protocol and credentials;
  custom endpoint settings are configuration data. A generic command, tool or hook
  cannot rewrite issued requests, headers, signed replay or active provider state.
  Replacement follows ordinary preflight and captured-binding rules.

Project executable contributions require an explicit trust decision binding their
manifest identity and digest to the canonical project root. Trust of an MCP file
must not silently approve a different command/hook manifest. Changed manifests
invalidate their own trust; merely opening a repository launches nothing new.
User-installed executables run with captured host permissions: this is not a code
sandbox. No registration protocol grants mutable Session or provider-wire access.
The first named-command protocol maps a manifest entry's program and fixed argv,
plus explicitly supplied literal arguments, to the existing user-shell action.
Quote/escape parsing groups arguments but performs no shell expansion; launcher
quoting preserves each argument. A command may intentionally invoke a shell or
another interpreter, so this is not a sandbox. Require an explicit context-sharing
choice in the manifest. Admission records the actual launched command and its
outcome through ordinary user-shell facts; no generator process runs outside
that custody and no stdout response is interpreted as a new control action.

Keep command catalogues separate from inert resources and model tool selection.
Only explicit client/Host command invocation executes them; prompt submission,
completion, listing, loading and reloading do not. Revalidate the captured source
manifest and its trust before each new launch; edits, deletion or revocation
reject stale definitions rather than selecting a replacement silently. Already
admitted work retains its captured route and stop/join custody. Reload preflights
resources and command metadata before publishing either. Project command trust
uses its own manifest identity/digest, not MCP trust. User-owned configuration can
register commands without project trust; scripts and transitive executable code
are not frozen by a manifest digest. Dynamic hook packaging, arbitrary extension
handlers and external renderers remain staged, not delivered by named commands.

## Context and providers

Keep committed facts, bounded model context and human presentation distinct.
Commit observed outcomes with their model-projection decisions. Delivery limits
must not rewrite successful effects as failed raw observations. Reopen, fork and
compaction use the recorded projection. Failed result commits leave unknown
effects, not fabricated outcomes. Prose and summaries are claims, not verification.

Context projection, branching and compaction have one owner. Compaction changes
context, not history; commit complete call/result cuts atomically and retain the
old projection when cancelled before commit. Preserve retrieval coordinates without
replay or implicit evidence injection. Summary requests frame historical records as
data and place the current summary task after all records and typed images; charge
that framing in prefix admission. These cues do not establish semantic truth or
prompt-injection resistance. Measure continuation quality, not compression alone.

The coding application supplies a read-only `history` tool for recovering committed
evidence after compaction or withheld output. Other applications opt in explicitly;
ordinary frozen catalogue selection applies. Core resolves reads against the live
invocation's Session, never a model-supplied database path or foreign Session ID.
Search/list returns stable one-based coordinates in the full committed entry
sequence and bounded previews; omitted entries can leave gaps. This does not hide
the existence or count of omitted records. Private commands and output remain
excluded. Read-by-entry returns UTF-8 byte pages with explicit continuation. Provider call
IDs are metadata, not unique retrieval identity. Search work and returned bytes
are independently bounded, with progress coordinates rather than a false complete
result when a scan stops early. Reads use a committed upper bound, preserve
observed success/error versus unknown/not-dispatched outcomes, and never dispatch
historical calls.

Retrievable records contain historical conversation text and tool evidence, not
provider replay, human thinking, image bytes, private direct-shell commands/output
or application/context configuration. Omitted media are identified, not encoded
into text. Compaction identifies its covered selected-path prefix and global end coordinate; withheld output
has a stable locator. Guidance names `history` only when the frozen loadout makes
it available. Reopen and copies use their actual committed history; inherited
coordinates confer no execution authority. Historical observations and summaries
are not proof of the current workspace, semantic fidelity or permission to repeat
an effect. This uses the existing Session store, not another result ledger.

Neutral prompt estimates count text/tool content once, excluding opaque
provider replay; retained request-representation byte bounds still include it.
These estimates are neither provider-token counts nor transport-wire bounds.
Use compatible observed input usage to inform later estimates; do not reuse a
sample across incompatible model routes, context representations or image inputs.
Missing usage must remain unknown, and calibration cannot relax independent
retained-byte or transport bounds. Samples are process-local per Session and
bound to the live model service, effective model and request-context scope.
Model/context/reasoning changes, compaction and replay rebasing invalidate them;
reopen and copied Sessions start cold. Record samples only from committed normal
assistant exchanges: total input plus that assistant's output establishes a
known context boundary, then estimate only subsequently appended messages. Do
not add cache sub-buckets again, recount the baseline assistant, or scale by
retained JSON that may duplicate opaque replay. Unknown output/opaque suffixes
require fallback rather than invented hidden-token counts. Summary and cache-warm
usage belong to different requests.
Provider-only continuation remains an adapter concern. Context bounds do not imply
bounded resident history or artifact retention.

Prepare wire declarations and durable context from the same catalogue. Admit
model/context changes atomically before issuance. Pre-output retries retain that
preparation; compaction/reset prepares again. Reserve 8192 tokens of estimation
headroom, capped at half a declared window, and use that same usable capacity for
request admission, summary sizing and recent-context retention. Clamp output
ceilings to remaining capacity. Custom routes may declare positive context/output
limits; Host persists them with the route and captures them in the bound agent,
including after reopen.
Unknown custom context stays unknown. Catalog limits cannot be overridden under
the catalog identity. Exhausting the dispatched output budget is not itself context
pressure.
Recovery is bounded and visible; incompatible context must fail or change explicitly.
A pre-output summary context rejection may shrink to strictly smaller settled-prefix
cuts on the same model/authority. Report each attempted reduction; stop when no
smaller cut fits or on cancellation. Do not retry a failed attempt after generated
text, thinking, tool fragments or opaque replay, or treat output truncation as
capacity rejection. Validate and commit each successful summary atomically; later
failure preserves that last committed projection rather than rolling back earlier
successful chunks.

Host prepares replacements before publishing them. Failed preflight leaves the
previous binding usable; active operations retain captured configuration. An idle
live operation gate permits future preference changes even when reopened history
contains an interrupted Turn. Such changes neither settle that Turn nor rewrite
its original configuration. Logical-Turn continuation still requires matching current
authority and configuration; passive recovery eligibility is not that attestation.
Logical selection, effective model, returned identity and usage remain distinct. Model
epochs survive reopen, including A → B → A; routing policy is not correctness state.

Adapters own framing, errors and opaque provider continuation. Recoverable error
identity must not depend on clipping its diagnostic presentation. Bound transport/
framing work separately from newly retained decoded output and replay; comments,
repeated snapshots and accounting alone do not replenish progress allowance.
Preserve or reject opaque data explicitly rather than truncating it to fit.
Human-readable
thinking differs from signatures/encrypted replay and returns to providers only
through adapter-owned replay. It counts as observed output whether displayed or
not. Preserve signed prefixes or report reset/incompatibility; never reset inside
an outstanding signed exchange. Destination adapters may alias unsigned migrated
tool IDs, pairing calls/results through their original identities without changing
committed facts. Allocate collision-free aliases without rebinding earlier encoded
prefixes; retained native identities remain opaque and fixed, or fail explicitly
on an unresolvable collision. Core must not branch on provider names.

Caching/affinity are optimizations, not authority. Affinity survives reopen and
changes on fork/clone. Warming is bounded, separately accounted and subordinate
to settlement. Schedule only positive refresh deadlines within the active horizon;
ineligible policies disable warming rather than issuing immediate requests.
Do not dispatch refreshes after cache expiry or the active horizon, even when
model responses or earlier refreshes arrive late. Transient provider retries are
bounded, cancellable and allowed only before observed output, without repeating
completed tools. Already-ready cancellation at a request-opening boundary wins
before even constructing the model service future, including normal, summary,
retry and cache-warm requests. Accounting, input-transformation notices and empty
deltas do not establish output custody.

## Clients and extension boundaries

Clients use typed shared control; RPC encodes it rather than defining a lifecycle.
Input custody is `draft → preparing → ready/queued → accepted | returned | withdrawn`.
State whether queue/acceptance promises are volatile or durable. Preserve original
kind, literals, attachments and reservations until acceptance or explicit handoff.
Validate before acknowledgement; a byte reservation is not a context/RSS guarantee.
Expand queued resources at admission. Steering joins a safe boundary in its Turn;
consume ready steering after asynchronous catalog refresh or automatic compaction,
before freezing the next request. Issued requests and their tool batches remain
frozen. Only explicit follow-ups start later Turns. Return late steering and
unaccepted inputs on failure/stop, including successful preparation racing cancellation.
Preparation remains cancellable/joinable without blocking controls; bounded active
decoders need not support hard interruption.

Owners and observers are separate. Dropping/timing out/aborting an owner requests
stop without relinquishing started-work custody. A service can retain and drive
that owner independently of transport connections. Observation loss/detachment
cannot decide execution lifetime. Foreground clients may stop on genuine I/O loss,
but saturation is backpressure, not execution failure. Reserve lifecycle/recovery
capacity before effects; do not use unbounded deferred replies or put cancellation
behind output.

Remote attachment must identify the Session/work and provide an authorized
snapshot plus an ordered committed revision/cursor or equivalent reconciliation
contract. Include relevant lifecycle/queue/settlement state, not just model deltas.
Live progress may be lossy; gaps require resync, never suffix-splicing. Attachment,
transport acknowledgement and snapshot access do not grant execution authority.
Applications may implement authenticated network transports over these shared
operations; neither a required daemon nor a second client loop is implied.

Headless text publishes committed final text. Protocols distinguish acceptance,
progress, settlement and stop intent without exactly-once transport promises.
Broken output cannot promise delivery; committed inspection remains available.
Controlled foreground EOF closes admission and stops/joins before final recovery
publication. A healthy final pipe still requires a reader.

Shutdown closes admission before stop/join. Cancelling its wait retains closure,
handles and failures; finish it before destroying the runtime. Drop cannot promise
async cleanup. A genuine task panic blocks live owner reuse and reports unknown
effects. No synchronous idle guard crosses await.

### Questions, approvals and terminal attachment

These are distinct application interactions, not new Core Turn states. Ordinary
clarification already uses a final question followed by new input in the same
Session. A same-Turn question capability instead exposes one live, correlated
request through shared Host control and returns the answer as its normal tool
observation. Bind it to the Session, Turn, tool occurrence and authorized responder;
reject stale answers. Question cancellation is an explicit result, not a fabricated
answer. Turn stop or responder loss releases the waiter without new dependent
dispatch. Answers are untrusted data, never steering, credentials or permission.
Without a responder, omit the capability or return explicit unavailability; do not
auto-answer. Terminal and bidirectional RPC coding connect an actual responder
before admitting work; one-shot headless execution has no answer channel.

`ask_user_question` accepts one batch of 1–4 questions, each with 2–6 labeled
options and optional descriptions. Optional `multi_select` defaults to false;
true permits an explicit set of unique option indices, including an empty set
meaning none rather than cancellation. Replies must match the question's mode;
labels and canonical request order come from the immutable live request, not the
responder. A separate free-text answer replaces all choices in either mode.
Multi-select toggles remain unsent UI state until explicit Next confirmation;
model-authored options cannot impersonate Next or Type something. Bound request,
option and answer sizes, validate against the exact live request, and accept the
whole batch atomically. Only one batch may
wait on a control owner; concurrent asks fail explicitly rather than queue behind
an unseen dialog. A non-clone responder lease and request nonce authorize the
reply; display coordinates or copied origins alone do not. The optional question
port is supplied by application configuration and retained across idle binding
replacement, never implicitly inherited by managed children. A new binding
invalidates old request identities. One-shot headless and managed coding children
do not declare the tool. Existing explicit tool selection also applies in RPC;
unattended clients can exclude `ask_user_question`.

Esc cancels the questionnaire with an explicit ordinary tool observation; it need
not stop the Turn. Ctrl-C stops the Turn. Losing a responder while a request waits
requests Turn stop before releasing its answer waiter. Composed calls retain that
owning Turn stop separately from a narrower guest-local cancellation token.
Stop, shutdown and control
loss revoke pending requests independently of a retained UI handle. Answers become
only committed tool observations, not new user/steering messages. Pending request
state is live Host custody, included in complete authorized pull inspection but
not a durable Session ledger; change notifications only invalidate that view.

RPC holds the same non-clone consumer lease for the foreground connection.
`question_changed` is a coalesced invalidation; `get_question` pulls the current
request or null through the shared inspection lane. The request carries its nonce
and Core origin, not copied answer authority. `answer_question` submits one typed
answer per question against that nonce; `cancel_question` cancels only the batch.
Reject malformed, stale and duplicate replies without changing the live request.
These replies use reserved control publication capacity rather than bulk reply
slots; a capacity refusal has no answer/cancel effect. Success acknowledges live
handoff, not durable tool-result commit or Turn completion. Retain at most one
pending invalidation under output pressure, independently of lost model progress.
EOF or I/O failure follows ordinary foreground stop/join; reconnecting does not
restore a dialog or authorize an answer from historical provenance.

The question view preserves the draft and defers to an existing picker, detail
view or attached PTY until that owner releases input. Its selected choice or text
editor stays visible in the actual viewport. A real displayed question may publish
blocked/question status without embedding its text; closing it restores the
underlying operation/outcome status. No approval state is implied.

An optional approval policy decides a specific validated, admitted invocation
before its executor's effect dispatch. Denial starts no effect and records that
outcome; cancellation or loss of the decision channel fails closed. Approval
cannot add tools, widen captured authority, change the approved arguments or
retroactively cancel a started effect. Keep remembered rules in application policy,
not a second Session ledger. The coding preset currently uses host permissions;
adding approvals is neither a sandbox nor a universal V1 requirement.

Human terminal attachment extends the existing process owner through shared
control. A trusted client acquires one live, opaque input lease scoped to the
current Session, captured cwd and PTY handle, without manufacturing a model tool
origin. The driver quiesces model input, acknowledges any delivered prefixes and
rejects undelivered queued portions before granting the lease. Model writes and
resize are rejected while leased; model reads and observation remain available.
Automatic protocol replies remain driver-owned. A stale lease cannot write or
release a newer one. A write lease grants neither secrecy nor lifecycle authority.

The terminal attachment view uses the existing screen projection, cursor and input
modes with no local echo or second emulator. It owns ordinary key input, including
Ctrl-C; an explicit detach key restores the composer. It may resize through the
same driver and reports projection/resize failures rather than showing stale state
as current. Detach, view failure and Session replacement release the lease without
stopping the program. Turn stop does not stop a program or revoke human ownership;
process exit/close and application shutdown end the view. Leases are not durable,
restart attachment or committed keystroke history. Headless clients do not attach;
RPC requires a typed authorized attachment consumer, not raw terminal passthrough.
An attached view and a question/approval dialog cannot both own keystrokes: make
focus transfer explicit before presenting another interaction. Implement each
interaction with its real caller before adding status variants or capability flags.

## Terminal experience

Inline native scrollback is primary; fullscreen is another viewport over the same
semantics. OSC 7501 root status is derived from typed operation/client state and
emitted by the physical publication owner; a report replaces the root record,
repeats stable application identity, and contains no task text, guessed progress
or invented blocked state; an actual masked credential prompt publishes
`blocked` with `kind=auth`. Derive activity from current foreground control,
including preparation, separately from retained settled outcomes. Returning
unadmitted input or closing credential entry restores that outcome (initially
idle), not stale working state; observed shell errors are errors, cancellation
is idle. Interactive terminal publication only; headless, RPC
and redirected output never carry these reports. Prefer restrained fx-like
hierarchy and progressive disclosure to tool
cards or a flat stream. Prompts, prose, activity, mutations and notices must remain
legible without color. Nested activity uses recorded parentage, not an invented
process tree. Queued is not running or successful.

Keep drafts editable during work: multiline/history, paste/images, external editor,
steering and follow-ups. Commands/pickers retain unsent input. Draft replacement
invalidates draft-bound completion ranges and cancels their discovery without
abandoning the worker's join. Text editing has bounded terminal-local undo/redo
and kill/yank recovery, independent of durable input custody. Ordinary typing
coalesces at word boundaries; paste, completion and external-editor replacement
are atomic edits. Undo restores literal text and its byte cursor, never submits
input or reverses admitted work. Submission/queue handoff, history/returned-draft
replacement, Session replacement and attachment changes start a fresh undo
baseline; changing views or receiving progress does not discard the unsent draft.
Attachments are not reconstructed by text history. A terminal-local kill ring
contains text only; consecutive directional kills accumulate in cursor order,
explicit yank can reuse text in another draft, and yank-pop is valid only directly
after yank/pop without intervening input actions. Failed over-limit edits change
neither text nor recovery history. Bound retained history and kill text separately,
evicting oldest entries rather than making the current draft invalid.
Budget activity and composer together; keep state/exceptions visible with honest omission/inspection.
Coalesce benign repetition, not mutations/failures. Details retain arguments,
observations, capture paths and delivery notices. An explicit copy action in a
selected tool's detail view copies that call's recorded evidence independently
of viewport width or scroll, not a different answer or the whole conversation.
Keep capture/projection omissions, child-context labels and image metadata;
never read referenced files or rerun work to complete the copy. This is client
presentation through existing clipboard custody, not new Session input. Preserve
the draft and inspector on success or failure, and distinguish a terminal
clipboard request from confirmed native-helper success.

Use one control-safe grapheme/display-width policy for content; composer byte/cursor
mapping is separate. Standalone zero-width units use a visible one-column replacement,
so measurement, wrapping and physical grid alignment agree; combined graphemes stay intact. Semantic styles survive redraw and irreversible publication. Assistant Markdown
tables wrap recognized cells with header emphasis and column alignment; narrow
viewports stack header/value fields rather than clipping values. Share inline
styles, link/control safety and grapheme wrapping with ordinary assistant text;
keep original model text in Session facts, source inspection, copy and export.
Assistant fenced-code syntax styles are presentation-only: use bundled grammars
selected by an explicit language token, preserve multiline state within the block,
and never infer a language, read a source path or fetch definitions. Bound synchronous
grammar work without omitting code; unsupported or over-limit blocks stay literal.
Token spans must pass through the same control-safe grapheme wrapping and physical
publication as ordinary text. User prompts and tool observations remain literal.
Publish settled content once. Terminal-local command output and diagnostics share
an ordered presentation log anchored to stable Session prefixes, not a clipped
composer surface or synthetic Session/model facts. Inline publication acknowledges
that log only after successful physical paint; fullscreen renders the same order.
Explicit command responses repeat on request; only consecutive identical diagnostics
at an unchanged boundary coalesce. Retain this display log until Session replacement
or client exit. Initial inspection and later reconciliation use the same stable
prefix; unfinished facts remain mutable previews, not publication anchors.
A closed group need not be final. Native reflow belongs to the emulator; resume
publishes a bounded recent tail with earlier facts inspectable.
Resize, scrolling, viewport changes and exit preserve composer/terminal custody.
I/O failure stops input/redraw and awaits settlement before reporting; panic/crash
cannot promise that. Physical publication is never execution authority.

## Hybrid composition

Code Mode is opt-in beside direct calls/discovery. A confined Host VM gets owned
code, limits and a bounded bridge; Core retains Session/catalogue authority. It is
not another agent loop or a durable JavaScript runtime. Pi is the model-facing
authoring reference: named discoverable calls, useful declarations, selected
structured/text/image output and actionable errors. The
[README](README.md#optional-code-mode) states the implemented subset;
no comparative speedup is assumed. VM execution capacity must be independent
of host work awaited through its bridge. Core retains an owned join future until
actual worker exit, not just receipt of a guest result; VM teardown precedes that
completion. Cancellation does not detach either the VM or its started host work.

Identify a parent by committed assistant-entry sequence and call ordinal, not a
reusable provider ID. Commit nested tool intent before dispatch and observation
before guest consumption. Parent settlement joins its tool children; recovery
closes unknown children without rerunning code. Selected results reach model
context; raw child facts stay inspectable, not fabricated conversation messages.

Guest failure exposes a stable parent locator for bounded metadata/JSON inspection.
Inspection preserves observed/not-dispatched/unknown, excludes private shell
context, opaque replay and normalized image bytes, and grants no replay authority.
Capture paths may disappear; saved observations, host state and missing evidence
are distinct.

Normal return drains transferred requests and started effects. Failure, cancellation
and budget/storage faults close admission and request stop while retaining futures
and joining the VM. Failed observation commits block consumption/dependent dispatch,
even when guest code catches errors. VM limits, native resources, capture/audit
and delivery have separate owners; VM isolation does not sandbox host tools.

## Qualification

Qualify the shared engine through coding and at least one restricted non-coding
embedding rather than assuming terminal success proves reuse. Durable requests,
logical continuation, attachment and foreground/background delegation need their
own crash/cancellation/redelivery evidence before being advertised.

Preserve distinct integrity/concurrency/cancellation/recovery protections; prune
obsolete fixtures and private-call assertions. Exercise changed built clients and
repository gates. Source, mocks/PTY, live tasks and native-emulator/human review
establish different things. Neither green tests nor reference similarity certifies
performance, model adherence, terminal stability or daily-driver readiness.
