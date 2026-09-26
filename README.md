# Ion

Ion is building a provider-neutral Rust coding agent with a first-class terminal
interface. It runs one primary conversation by default, with optional cooperating
worker conversations. Pi/Pico and Codex are engineering references, not
compatibility targets.

## Current status

The workspace builds a headless and inline terminal `ion` executable and three libraries:

- `ion-core`: the replacement durable Turn runtime and storage layer.
- `ion-ai`: provider-neutral model contracts and scripted provider fixtures.
- `ion-terminal`: low-level terminal components.
- `crates/ion-app`: a headless and inline terminal host using the same Session path.

The maintained `ion-core` no longer contains the prototype Session/task/tool/workspace
runtime. The replacement branch implements the R1 durable domain, Session/provider
foundation, and an initial tool-execution boundary:

- fresh SQLite schema v12 with one Session-local identity sequence and exact commit cursor;
- revisioned conversation configuration, conversation-scoped idempotent input admission,
  inline immutable `TurnEnvironment`, constrained `TurnSettings`, and one unfinished Turn
  per conversation;
- a dedicated bounded SQLite command thread with WAL, `synchronous=FULL`, foreign keys,
  strict durable decoding, and mutation fencing after ambiguous persistence;
- semantically passive `Session::open()`: reopening performs no provider/tool
  reconciliation, dispatch, retry, timer work, worker start, workspace claim, or recovery write;
- exact `CommitReceipt { seq, update }` publication and bounded subscribe-before-snapshot
  observation with overflow/resnapshot semantics and paginated older history;
- explicit supervised `resume()`, typed `DriveExit`, process-local `SessionHealth`,
  per-Turn effect gates retired after terminal settlement, and cancellation generation
  linearization;
- logical `ModelStep` versus physical `ModelAttempt`, durable response-ready evidence,
  monotonic start-receipt/evidence refinement, selection guarded by current Turn generation and
  step eligibility, and atomic predecessor-superseding provider fallback;
- automatic safe-boundary context compaction through a tool-free ModelStep. A complete,
  bounded advisory text checkpoint and raw exchange tail become one immutable
  ContextBoundary; exact current-turn user input remains separate, and old transcript
  evidence remains inspectable. Empty, oversized or incomplete checkpoints park without
  advancing the boundary;
- stable provider effect keys derived from Session + Turn + step ordinal and covered by the
  provider-request fingerprint when adapters use them as idempotency material; exact frozen
  service-realm matching and host-owned credential/egress preflight before intent, with a
  second live check before adapter start;
- frozen tool-schema validation and persisted `PreparedAction` admission; digest-bound
  action authority checked against the frozen ceiling before execution intent; distinct physical
  tool attempts, conservative receipt recovery, nonoverlapping retries, durable outcome staging,
  source-order results, and model-context/storage closure reserves before execution;
- explicit unknown-result acceptance without rewriting execution evidence, late reconciliation,
  and cancellation that closes the tool exchange without claiming uncertain work stopped;
- a host-owned cross-process `workspace_registry` outside the checkout, with frozen Unix
  filesystem/repository identity, durable mutation claims, revision checks, and orphan
  quarantine that survives Session/blob deletion. Trusted hosts authenticate resolution
  evidence; this coordinates cooperating writers, not confined execution.

Opening and inspection are passive; explicit resume is the boundary that may reconcile a
persisted provider attempt and start new provider work. Closing seals local effect admission,
signals and joins locally owned drive work, then releases storage ownership without silently
turning suspended work into user cancellation.

`resume_with_tools` accepts exact compatible host tool implementations; missing bindings
park before a new provider dispatch. After a completed provider response is durable, a
missing exact preparer instead records an unavailable invocation and a source-order error
result without fabricating an action or physical attempt; subsequent requests park until
the selected loadout resolves. Invalid model tool arguments become a separate
source-order error without an attempt, so the model can correct them on continuation. The host tool boundary checks live authority before
execution intent; denial parks without spending a physical attempt. It must recheck at
actual effect admission because permission can change between those points. `tool_records`
inspects attempts, `accept_tool_unknown` settles an uncertain exchange, and
`reconcile_tools` recovers evidence without dispatching. Live host policy returns
`Allow`, `Ask` or `Deny`: `Ask` parks on a durable per-invocation approval bound to its
exact action, executor, workspace and expiry. An authenticated host can call
`decide_tool_approval`; explicit denial stages a truthful result without an attempt.
Duplicate decisions do not advance the commit cursor. Active tool records are included
in bounded snapshot/watch hydration.

Tool execution is sequential. Scripted tests cover an actual owner-process kill after a
filesystem mutation and host receipt, followed by passive reopen and explicit reconciliation
without reexecution. These fixtures are not native tools or a confinement implementation.
A host backend must enforce current authority, workspace claims, and stop/join behavior.
The deleted `.ion/claims.sqlite` wrapper is not part of the replacement runtime.

There is no compatibility bridge or hybrid old/new runtime. Earlier unreleased schemas
(v1–v11) are refused rather than migrated; Git retains the prototype and its useful failure
scenarios are being restored against the replacement owners.

The CLI now exposes bounded `list`, `read`, `create`, and exact-base `edit` tools,
an opt-in Linux `exec` tool, and an inline terminal client with transcript display,
bounded live model-text and command-output previews, and Ctrl-C cancellation.
Two synthetic workspace tasks completed against a local Qwen model through the
OpenAI-compatible loopback endpoint: read/edit/re-read and list/read/create/re-read.
A terminal session also completed a live model exchange and restored the terminal.
One synthetic Linux task also created C source, compiled and ran it with native
GCC through `exec`, and read the imported source back. A Fedora local-Qwen
two-turn C task fixed a parser, added regression tests, then added and tested a
formatting API in the same Session across context boundaries; independent host
tests passed. An opt-in terminal approval flow has synthetic PTY coverage and
local-model create/read and exec/edit/exec coding checks on Fedora;
native macOS command execution and broader provider qualification remain open.
Parallel tool dispatch, context reset/forking,
Steer/InteractionReply placement and workers remain optional later work.

With the text checkpoint, a separate Fedora PTY run completed a `make test`
Turn and a test-edit/`make test` Turn in one Session, crossed three context
boundaries, and exited cleanly. The model's first guard test was ineffective;
a corrective headless Turn in the same Session fixed the guard and completed
after three more boundaries. Independent `make test` passed and production C
files were unchanged. This demonstrates recovery from a model mistake, not
general test-design reliability.

Known limits before prerelease: absolute Turn wall deadlines are not implemented;
the library now rejects non-`None` deadlines at admission and the CLI supplies
`None`. Previously persisted experimental deadlines are not retroactively
enforced. The host freezes the selected wire API and canonical endpoint in its
provider binding. Sessions created by earlier experimental
headless builds with unscoped binding IDs require a fresh state directory.
Tool admission now assigns a durable per-call result allowance from the actual
batch and remaining context headroom, rather than reserving 64 KiB for a tiny
read. It still compares serialized request **bytes** to an asserted input-token
capacity, not an exact tokenizer-backed bound; a provider may reject a context
that passes this conservative proxy.
Compaction uses the selected frozen provider and stores one complete, bounded
advisory summary. A local Qwen run with the earlier JSON checkpoint exhausted
an asserted 4096-token output cap; the drive parks incomplete responses as
`IncompleteResponse`. With the text checkpoint, a fresh 4096-cap two-turn C
task completed across two boundaries and passed independent `make test`; an
8192-cap two-turn task also passed. These are task observations, not universal
output-cap recommendations or provider guarantees.

`submit_turn` atomically admits text and places its Turn with one watch receipt;
request-key replay is idempotent and an insertion fault rolls back the submission.
Unimplemented Steer/InteractionReply inputs reject at admission rather than queue
unconsumable work. Tool results distinguish complete inline, complete artifact and
incomplete capture. An oversized backend value becomes an explicit incomplete output
warning, preserving its terminal effect evidence without permitting replay. A
Session-owned bounded BlobStore now publishes auxiliary complete output through an
attempt-scoped capability. Settlement atomically links the verified object to the
physical attempt; Session reads page committed refs and return `ContentUnavailable`
for missing/corrupt auxiliary bytes without rewriting the result. Explicit GC excludes
publication through the queued SQLite commit. Only this auxiliary tool-output use is
wired; there is no automatic Session-namespace deletion or native large-output producer.
The host must keep that namespace outside agent-writable workspace state and protect
its filesystem ancestry from untrusted same-user processes. Session ownership locks
resolve database symlink aliases; hard-linked database aliases are refused.
A registry-authenticated native `read` ToolBoundary supports bounded file ranges and a
persisted tool exchange. A complete read from offset zero returns a SHA-256
`base_digest` for exact edits and a before/after-checked registry revision.
Partial reads return no base digest. The `native-read-v4` binding fits UTF-8
content to the actual serialized result allowance, allowing a complete edit
digest when ordinary source text fits; larger or heavily escaped content still
returns a bounded partial result. In one live local-Qwen check with an asserted
8K input limit, `read` returned a complete 2,299-byte C file and digest inside
a 4,468-byte result allowance. It is serial, not an OS sandbox: the host must
protect the workspace namespace against concurrent renames and enforce its
promised read authority. Native `list` returns a bounded, sorted
page from one directory with a workspace revision; it does not follow symlinks
or promise a snapshot across pages.
The library also has an experimental single-file `NativeEditBoundary` on registry
format v6, with protected private staging, pre-create allocation, immutable receipts,
joined worker custody, durable rename phases and discoverable cleanup obligations.
Mac and Linux ARM guest synthetic process-loss/Session recovery tests pass; they do
not prove host confinement or power-loss durability. A trusted host must continuously
protect the registry, custody inode and staging namespace from arbitrary same-user
writers; `0700` and file locks alone do not establish that protection. Blocked
allocations have no force-clear. The host can opt into edit and create with
an explicit shared registry. Creation requires an absent target and uses an
atomic no-clobber rename. A narrow two-Turn Fedora coding task passed; broader
repository and provider qualification remains open.
Registry v3/v4/v5 files are refused without migration.
Git marker discovery refuses symlinked/nonregular marker files rather than opening them.
The OpenAI-compatible Chat Completions adapter streams text, function calls and usage
with bounded SSE parsing. It binds to a frozen HTTPS or literal loopback HTTP origin, disables redirects and ambient
proxies, and obtains an API key from a live host callback at dispatch. Returned-model IDs
must match the exact binding or a frozen list of allowed route models; missing or unexpected
IDs park without selecting a response or admitting tools. Returned calls also must
respect frozen tool choice and parallel-call controls, including after passive reopen.
Credentials and provider preflight are neither network confinement nor a
production credential policy. Library hosts can supply a conservative,
route-wide cost bound for each proposed model attempt;
the store reserves it with attempt intent and retains it unless the attempt is proven
not started. A configured cap parks before intent without a trusted quote or enough
remaining allowance. The headless host accepts an optional operator-asserted
all-in per-attempt quote and frozen Turn cap; it does not discover provider
prices or infer exact billable tokens from request bytes. Request and terminal
provider-response capacity checks stop encoding at their frozen limits rather
than allocating complete oversized JSON copies. A separate Anthropic Messages
adapter supports streamed text and client tools, with strict
index/terminal/usage checks and stable logical tool-result pairing. Unsupported thinking,
opaque replay, provider-hosted tools and explicit sampling/reasoning controls fail closed.
Isolated synthetic OpenRouter Chat Completions exchanges passed; neither official
provider API has been qualified live.

## Terminal and headless use (experimental)

`cargo run --locked -p ion -- --help` exposes interactive `chat`, headless `run`,
`resume`, and passive `inspect`. `chat` uses the same durable Session and accepts
multiline paste; Enter submits, Shift-Enter inserts a newline, Ctrl-C requests
cancellation, and `/resume` retries an unfinished Turn. Model text appears as a
bounded provisional preview while a response streams; completed content comes
from the durable transcript. Linux `exec` stdout and stderr also appear as a
bounded provisional preview while a command runs. It requires a terminal.
Create a host-state directory outside the writable workspace, then supply an exact
HTTPS Chat Completions endpoint or literal loopback HTTP endpoint and a model ID.
The host must assert the model's
input and output token capacities; the client cannot discover or verify them.
The current request admission caps serialized bytes by `--max-request-bytes`
(default 1 MiB) and the asserted input-token capacity. This conservative byte
proxy is **not** a tokenizer-backed bound; a provider may still reject a request.
The per-request output cap defaults to the asserted model output capacity;
`--max-output-tokens` can narrow it.
The endpoint's returned model ID must match the supplied ID. `run` supports
`--request-key` for idempotent resubmission after a lost reply. Resuming with
a different endpoint path—even on the same origin—is refused.

```sh
mkdir -p "$HOME/.local/state/ion/example"
export OPENAI_API_KEY='your provider key'
cargo run --locked -p ion -- run \
  --state "$HOME/.local/state/ion/example" --workspace "$PWD" \
  --endpoint 'https://api.example.com/v1/chat/completions' \
  --model '<exact-model-id>' \
  --model-input-limit '<model-input-tokens>' \
  --model-output-limit '<model-output-tokens>' \
  --request-key example-1 'Read the project entry point and summarize it'
```

Use `chat` in place of `run` for the terminal client, with the same host and model
arguments. Replace the example endpoint and capacity placeholders with values
for your provider.
Add `--ask-mutations` to `chat` to review each `edit`, `create` or `exec`
PreparedAction before execution. The terminal prints the complete frozen action
and its digest; type `/approve <digest>` or `/deny <digest>` to decide that
invocation. A changed or incomplete review cannot authorize execution. This
host policy applies to the current chat process; pass the flag again when
reopening the Session if you want the same policy. An already pending approval
still requires its exact decision after reopening. Headless `run` and `resume`
do not request new interactive approvals.
For a Turn-wide reservation ceiling, set `--max-cost-microusd <positive-total>`
on `run` and `--cost-quote-microusd <trusted-per-attempt-upper-bound>` on `run`
and each `resume`. Both are in millionths of a US dollar. The latter must
**actually bound every possible physical request** under the frozen provider
route, including input/output, cache, reasoning, fees and prices before
provider start; it is an operator assertion, not catalog discovery, token
counting, provider billing or a guaranteed bill cap. Without a quote a capped
Turn parks `CostQuoteUnavailable`; when the entire asserted bound does not fit
its remaining allowance it parks `MonetaryCapacity`, both before provider
start. Past attempt quotes remain immutable; unknown or completed effects keep
their reservations. The ceiling freezes with the Session configuration, while
the quote may change on resume. Do not configure a monetary ceiling without an
authoritative upper bound for your provider and route.

The default `--wire chat-completions` reads `OPENAI_API_KEY`. For Anthropic's
`/v1/messages`, use `--wire anthropic-messages`, an exact Anthropic endpoint/model,
and `ANTHROPIC_API_KEY`. Each Session freezes its wire API and canonical
endpoint URL; use a new state directory to switch endpoints. Keys are
read at dispatch and are not stored. Literal-loopback HTTP providers may run
without a key; public HTTPS providers park if their key is missing. `ion inspect --state ...` shows a bounded snapshot;
`ion resume --state ... --workspace ... --endpoint ... --turn <id>` explicitly
resumes a persisted Turn. A read-only Session uses `<state>/registry` by default.
To enable mutation tools, give every Session touching the same
workspace **one shared host-owned registry**, separate from its per-Session state
and the workspace. Create it privately on the same supported local filesystem
as the workspace, then pass `--registry <path> --enable-edit` and/or
`--enable-exec` to both `run` and `resume`. The tool loadout is frozen when the Session
is created; use a new state directory to change modes. The registry incarnation
is frozen into the workspace binding, so resuming with another registry fails.

```sh
mkdir -m 700 -p "$HOME/.local/state/ion/shared-registry"
mkdir -p "$HOME/.local/state/ion/edit-example"
cargo run --locked -p ion -- run \
  --state "$HOME/.local/state/ion/edit-example" \
  --registry "$HOME/.local/state/ion/shared-registry" \
  --workspace "$PWD" --enable-edit \
  --endpoint 'https://api.example.com/v1/chat/completions' \
  --model '<exact-model-id>' \
  --model-input-limit '<model-input-tokens>' \
  --model-output-limit '<model-output-tokens>' \
  'Read one file, replace the requested text, then summarize the change'
```

The current `native-edit-private-v6` tool edits existing regular files up to 16 KiB.
The model supplies the `base_digest` from a complete read, plus text that occurs
once and its replacement. The host verifies the digest, captures the current
workspace revision, builds the complete replacement and freezes both file versions
before creating a workspace claim. An unrelated earlier edit does not invalidate
an unchanged target file's digest. This reduces model output, but still needs
comparative coding-task evaluation: exact matching does not
prove that the model chose the right change. `create` takes an absent path and
content; the host captures the revision. New files are created with mode `0600`.
Both tools create private staging inside the shared registry,
refuse unsupported filesystem or mount combinations, and never fall back to
workspace staging.
The host must continuously protect the registry, its staging directory and the
workspace namespace from arbitrary same-user writers; directory permissions and
advisory locks are **not** a sandbox. Blocked edit allocations have no automatic
cleanup. Provider calls can send workspace content and incur charges.

On Linux, `--enable-exec` requires `/usr/bin/bwrap`, a private shared registry,
and a workspace and registry staging directory on the same supported local
filesystem. The command runs with no network in a fresh PID namespace against a
private copy of the workspace. Git-ignored paths and sockets/FIFOs are omitted;
included symlinks and hard links are currently refused. A command can inspect a
private copy of Git metadata, but changes to Git metadata are never imported;
the result marks a private Git mutation as an error even if Git exited zero.
After the command and its descendants stop, the host checks the captured bases
and imports at most 32 changed paths and 64 MiB of ordinary file content. The
private view is limited to 100,000 entries and 2 GiB. The result reports
`imported_paths`, omissions and any `import_error`; a nonzero command exit can
still import changed files. The import plan is durably recorded before ordinary
files are published. An owner crash retains an unresolved workspace claim and
private artifacts for reconciliation. The CLI has no automatic reconciliation
for that case yet. Native macOS `exec` is unavailable; Linux command success
does not qualify the first cross-platform prerelease.

For Rust work on Linux, pass `--exec-rust-toolchain <toolchain-root>` with
`--enable-exec`. Ion mounts that selected directory read-only at `/toolchain`
and uses its `cargo` and `rustc` with a private writable Cargo home and network
disabled. `--exec-cargo-registry <cargo-home>/registry` additionally exposes a
selected cached registry read-only for offline dependencies. Choose roots that
contain only toolchain/cache content intended for the command to read; Ion
rejects roots with sockets, FIFOs, devices or nested mounts and never mounts
Cargo credentials or configuration automatically. The selected root identities
are frozen in the Session's `exec` binding. The live Fedora Rust check compiled
a cached `serde` dependency, fixed a failing test and passed an independent
`cargo test --offline` run.

A synthetic OpenRouter Chat Completions read-and-answer exchange passed on
2026-09-25. On 2026-09-26, a local Qwen model over loopback completed isolated
headless read/edit/read and list/read/create/read tasks; externally inspected
files held the requested bytes. It also completed a read-and-answer exchange
through `ion chat` in a PTY without an API key. A Linux Bubblewrap scope and a
synthetic C creation/compile/run task passed on Fedora; the imported source and
binary were inspected outside Ion. A disposable Git repository task also passed:
with a 16K local-model context, Ion observed a failing C test, edited the parser,
read it back and reran the test; an independent host run passed. The same task
parked before editing with an 8K context because Ion conservatively reserves
serialized request bytes against the asserted input-token capacity. A live PTY
`ion chat` run used `exec` to write a file, read it back and exited through
`/quit`. A direct OpenAI attempt returned HTTP 429;
the Anthropic adapter also completed a synthetic loopback `ion run` read-and-answer
exchange through its Messages wire API. These checks do not qualify
public-provider behavior, macOS exec or the full
range of terminal emulators. The excluded legacy `crates/ion/` remains
reference material, not an alternative maintained runtime.

## Development

The checked-in toolchain pins Rust 1.98.0 and the required components.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
scripts/smoke.sh  # offline executable/Session preflight; not live-provider or PTY
```

The replacement runtime regressions currently live here:

```sh
cargo test --locked -p ion-core --test r1b_storage  # admission/config/Turn/store/watch
cargo test --locked -p ion-core --test r1b_drive    # provider drive/cancellation/recovery/fallback
cargo test --locked -p ion-core --test c1_tools     # tool exchanges/receipts/closure/process loss
cargo test --locked -p ion-core --test r1c_workspace_registry # claims/identity/process loss
cargo test --locked -p ion-core --lib               # domain/schema/request/observation contracts
```

These are deterministic library/fixture and offline executable checks; they are not
evidence of live-provider effectiveness or a usable terminal application.

## Project documentation

- [ARCHITECTURE.md](ARCHITECTURE.md): target contracts, ownership and failure semantics.
- [AGENTS.md](AGENTS.md): repository working instructions.

Earlier architectures and implementation history remain in Git. They are not
compatibility targets. Research notes and development planning are not public
architecture contracts.

## License

[MIT](LICENSE)
