# Provider request assembly

Status: proposed design for review; no implementation or validation claimed. Source baseline: `28b6c7a8` (2026-09-13).
This slice specifies the direction of `DESIGN.md` §7, §9–10, §13–14, not a new runtime. R7/R8 acceptance remains open (`ROADMAP.md:40-41`); the sequencing rationale is the holistic review (`ROADMAP.md:352`).
All Rust surfaces below are proposed replacements; signatures omit routine derives/imports and implementation bodies. Paths are repository-relative. Public API knowledge is labelled **PV**: a hypothesis requiring a versioned wire fixture, not verified vendor behavior in this repository.

## 1. Scope and non-goals

Define two boundaries: durable conversation configuration → reproducible neutral request in `ion-core`, and neutral request → one provider attempt in `ion-ai`.
Observable result: a headless conversation sends its selected instructions, model, controls, tools and ordered context; recovery sends the same request or refuses before networking. Providers cannot execute tools or mutate sessions.
Use OpenRouter's chat-completions dialect and the materially different Responses item/event protocol. Start live testing with one explicitly selected provider/model, not a catalog.
No speculative configuration framework, provider catalog breadth, credential storage, routing policy engine, alternate runtime, automatic cross-provider fallback or generic Effect lifecycle.
The active workspace excludes the legacy application (`Cargo.toml:1-3`); its adapters are source material, not working integrations. Selectively rebuild wire leaves, never restore their operation-bound interfaces (`crates/ion/src/openrouter.rs:21-24`; `crates/ion/src/openai_codex.rs:16-19`).
Execution enforcement and a terminal UI are separate boundaries. This document specifies the inputs/evidence they must supply and the joint acceptance task, not their implementations.

## 2. Contract

### `ion-ai`: request and ordered content

Keep `ModelRef { provider: String, model: String }` and `ToolSpec { name, description, input_schema: Value }` (`crates/ion-ai/src/model.rs:3-7`; `crates/ion-ai/src/tool.rs:4-9`). Provider names identify host registrations, not wire API families.
Replace the current three-field request (`crates/ion-ai/src/request.rs:5-10`) with the following. `ResolvedRequestSpec` lives in `request.rs`; controls in `controls.rs`; content/replay in their existing semantic modules.

```rust
pub struct ModelRequest { pub spec: ResolvedRequestSpec, pub messages: Vec<Message> }
pub struct ResolvedRequestSpec {
    pub model: ModelRef,
    pub instructions: String,
    pub controls: GenerationControls,
    pub tools: Vec<ToolSpec>,
}
pub struct GenerationControls {
    pub max_output_tokens: u32,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub reasoning: Reasoning,
    pub tool_choice: ToolChoice,
    pub parallel_tool_calls: bool,
}
pub enum Reasoning { ProviderDefault, Off, Low, Medium, High }
pub enum ToolChoice { None, Auto, Required, Named(String) }
pub enum Role { User, Assistant, Tool }
pub struct Message { pub role: Role, pub content: Vec<ContentBlock> }
pub struct ContentBlock { pub content: Content, pub replay: Option<ProviderReplay> }
pub enum Content {
    Text(String), ToolCall(ToolCall), ToolResult(ToolResult),
    Refusal(String), Opaque { summary: Option<String> },
}
pub struct ToolCall { pub id: String, pub name: String, pub arguments: Value }
pub struct ToolResult { pub call_id: String, pub name: String, pub result: Value }
pub struct ProviderReplay {
    pub origin: ModelRef,
    pub api: String,             // e.g. "responses-v1"
    pub format_revision: u32,
    pub content_digest: [u8; 32], // binds this block's neutral content
    pub raw_json: String,        // exact UTF-8 JSON value; never reserialized
}
```

**Do not add `Role::System`. Instructions are a separate field**, resolved once, outside transcript compaction/fork inheritance. This gives both system-message and top-level-instructions APIs one authoritative instruction source; transcript text never gains system authority. The current roles already exclude System (`crates/ion-ai/src/message.rs:6-11`).
`instructions` is one exact string: host-authorized base text followed by explicitly selected project instruction text, separated by `\n\n`, with no trimming. Ordinary retrieved files remain user/tool context. Instruction text is not execution authority.
`None` temperature/top-p and `ProviderDefault` mean deliberate wire omission, not “look up today's host default”; `Off` must disable reasoning or return Unsupported. Require finite numbers, temperature ≥ 0, 0 < top-p ≤ 1, positive token limit; model-specific ranges are adapter validation. No arbitrary provider-options JSON bag.
Preserve block order, exact strings, call IDs and opaque bytes. Opaque requires replay; only assistant blocks may carry replay/refusal/opaque or calls, user blocks are text, and tool messages contain only results. Tool JSON must be an object for arguments; results may be any JSON value.
The current replay is one message-level `Value`, with provider-only compatibility (`crates/ion-ai/src/message.rs:14-32,46-48`). Replace it with block-local origin/API/revision checks and a content digest; neither silently ignore nor double-emit replay. Accepted replay replaces the block's generated wire encoding, after validating that it describes that block.
For Responses, one neutral block represents one output item; a text item's ordered text parts concatenate for display, while its complete raw item retains part boundaries, annotations and item ID. Opaque reasoning occupies its original block position. Unknown semantic item types return Unsupported, not a fabricated text block.
Cross-provider/model replay is refused by default. Changing provider requires an explicit durable context replacement/reset that removes incompatible replay; any lost opaque content must be shown to the user. A provider switch is not permission to discard history.

### `ion-ai`: streaming, termination and usage (`response.rs`, `usage.rs`)

The current stream exposes only text, complete calls, usage and a final response (`crates/ion-ai/src/response.rs:37-43`). Replace it with indexed provisional blocks; only a validated final response can admit tools.

```rust
pub enum BlockKind { Text, ToolCall { id: String, name: String }, Refusal, Opaque }
pub enum BlockDelta { Text(String), ArgumentsJson(String), Summary(String) }
pub enum ModelStreamEvent {
    BlockStart { index: u32, kind: BlockKind },
    BlockDelta { index: u32, delta: BlockDelta },
    BlockEnd { index: u32, block: ContentBlock },
    Usage(Usage),
    Finished(ModelResponse),
}
pub struct ModelResponse {
    pub message: Message,
    pub termination: ResponseTermination,
    pub usage: Usage,
    pub provider_response_id: Option<String>,
}
pub enum ResponseTermination {
    Completed(StopReason), Incomplete(IncompleteReason), Refused { reason: String },
}
pub enum StopReason { EndTurn, ToolCalls }
pub enum IncompleteReason { MaxOutputTokens, ContextLength, ContentFilter, Other(String) }
pub struct Usage {
    pub input_tokens: Option<u64>, pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>, pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
```

Indices describe final order, not arrival order. Starts are unique; deltas require an open matching block; ends occur once. Buffer fragmented call identity until complete enough to start; never invent an ID or repair invalid arguments to `{}`. Bound both indices and buffered bytes.
`Finished` occurs exactly once and ends the stream; its blocks must agree with ended blocks and accumulated deltas. Final replay may arrive only at BlockEnd. `Err` is terminal; EOF without Finished is a protocol failure, not evidence of provider completion. Partial calls never execute, even if one block ended.
`Completed(ToolCalls)` requires at least one complete valid call and `EndTurn` requires none. Refused/incomplete responses are durable attempt evidence, not successful answers or executable calls. Keep incomplete/refused content out of model-visible history unless a later explicit policy appends it as attributed evidence.
Usage updates are cumulative snapshots, never increments. Merge reported fields; absence does not erase a prior report. Totals include cache/reasoning subsets, so never add subsets to totals. Unknown stays `None`, continuing the current distinction (`crates/ion-ai/src/usage.rs:4-8,20-25`). Reject contradictory counts as protocol failures while retaining the last valid usage evidence.

### `ion-ai`: provider and registry (`provider.rs`, `registry.rs`)

Replace `ModelService`, not wrap it with a second dispatch interface. Move the scripted implementation to `ScriptedProvider` with the same trait and deterministic fault scripts; today it records requests and emits fixed events/open errors (`crates/ion-ai/src/scripted.rs:8-16,32-53`).

```rust
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ProviderError>> + Send + 'static>>;
pub struct ProviderStamp {
    pub provider: String, pub api: String, pub implementation: String,
    pub endpoint_profile: String, // public, immutable host profile revision
}
pub struct PreparedRequest { /* private: stamp, body bytes, semantic headers, digest */ }
impl PreparedRequest {
    pub fn new(stamp: ProviderStamp, body: Vec<u8>, semantic_headers: BTreeMap<String, String>)
        -> Result<Self, ProviderError>;
    pub fn body(&self) -> &[u8];
    pub fn semantic_headers(&self) -> &BTreeMap<String, String>;
    pub fn stamp(&self) -> &ProviderStamp;
    pub fn digest(&self) -> [u8; 32];
}
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn prepare(&self, request: &ModelRequest) -> Result<PreparedRequest, ProviderError>;
    fn stream<'a>(&'a self, request: PreparedRequest)
        -> BoxFuture<'a, Result<ModelStream, ProviderError>>;
}
pub struct ProviderRegistry { /* BTreeMap<String, Arc<dyn Provider>> */ }
impl ProviderRegistry {
    pub fn register(&mut self, provider: Arc<dyn Provider>) -> Result<(), ProviderError>;
    pub fn get(&self, id: &str) -> Result<Arc<dyn Provider>, ProviderError>;
}
```

`prepare` is pure: validate model, instructions, every explicit control, tool schema, ordering and replay; serialize the complete semantic request with no credential reads or networking. Unsupported capabilities are typed failures here, not a list of optimistic booleans. Reject duplicate/empty registry IDs; unknown provider/model is Unsupported. No registration replacement while invocations use that registry.
PreparedRequest::new validates byte caps and an allowlist of nonsecret semantic headers, computes the digest, and exposes no mutation. Method is POST; route is fixed by the endpoint-profile revision. Prepared bytes are immutable and movable, tied to their originating provider stamp. `stream` verifies ownership, resolves host credentials, and makes **at most one generation HTTP request**. No redirects, reconnects, provider fallback or hidden SDK retries. Neither a request nor a stamp contains Ion session/task IDs.
Host instances live in optional `crates/ion-ai/src/provider/{openrouter,responses}.rs`; shared wire leaves in `crates/ion-ai/src/api/{chat,responses}/{request,stream,error}.rs` and `api/sse.rs`. Enable HTTP behind one feature, leaving contract/scripted builds HTTP-free. Do not pre-create auth/catalog abstractions.

### `ion-core`: durable configuration and assembly surfaces

Within `crates/ion-core/src/`, `conversation/config.rs` owns configuration; `builtin/request.rs` owns assembly; `builtin/attempt.rs` owns frozen/attempt evidence. `session/config.rs` owns writer validation/CAS. These are proposed additions, not general extension points.

```rust
pub struct ConversationConfig {
    pub model: ModelRef, pub instructions: String, pub controls: GenerationControls,
    pub tool_names: Vec<String>, pub context: ContextPolicy, pub limits: RunLimits,
}
pub struct ContextPolicy {
    pub max_request_bytes: u32, pub max_input_tokens: u32,
    pub compact_at_tokens: u32, pub summary_max_tokens: u32,
}
pub struct RunLimits {
    pub max_model_steps: u32, pub max_attempts_per_step: u32,
    pub max_cost_microusd: Option<u64>, pub deadline_ms: u64, // duration from turn admission
    pub max_response_bytes: u32, pub max_tool_output_bytes: u32,
}
pub enum ContextCut { Empty, Through(EntryId) }
pub struct AssemblyBasis {
    pub cut: ContextCut, pub config_revision: CommitSeq, pub config: ConversationConfig,
    pub placed: Vec<(InputId, EntryId)>,
}
impl TaskContext {
    pub async fn request_basis(&self) -> Result<AssemblyBasis, TaskContextError>;
    pub async fn request_entries(&self, cut: &ContextCut, after: Option<EntryId>,
        limit: usize, byte_limit: usize) -> Result<EntryPage, TaskContextError>;
}
impl TaskDriver {
    pub async fn configure_conversation(&self, id: ConversationId,
        expected: Option<CommitSeq>, config: ConversationConfig) -> Result<CommitSeq, SessionError>;
}
pub async fn assemble(context: &TaskContext, providers: &ProviderRegistry,
    tools: &ToolCatalog) -> Result<(FrozenGeneration, PreparedRequest), AssemblyError>;
pub enum AssemblyError {
    MissingConfiguration, InvalidConfiguration(String), Context(ContextError),
    Provider(ProviderError), Read(TaskContextError), LimitExceeded(String), ReplayMismatch(String),
}
```

## 3. Two wire mappings

PV verification targets: [OpenRouter requests](https://openrouter.ai/docs/api/reference/overview), [OpenRouter streaming](https://openrouter.ai/docs/api/reference/streaming), [Responses](https://platform.openai.com/docs/api-reference/responses), and [SSE framing](https://html.spec.whatwg.org/multipage/server-sent-events.html). These are reference targets, not sources fetched or fixture-verified during this document's authorship.

### Chat completions: OpenRouter profile

Source evidence: legacy request/tools/instructions are encoded in `crates/ion/src/openrouter.rs:99-113,220-236,460-506`; bearer auth and cancellable send at `:239-275`. It sends `reasoning.effort`, not generic OpenAI `reasoning_effort`. This design gives that dialect its own profile; “OpenAI-compatible” does not imply identical controls.
Encode instructions as the first system message (including an explicitly empty string), then ordered messages. Tool specs become `tools[].function.{name,description,parameters}`; calls retain provider IDs and encode arguments as canonical JSON strings; tool results use `tool_call_id` and canonical JSON text as content.
**PV:** send `max_tokens`, temperature, top_p, tool_choice and parallel_tool_calls only using the selected model/profile's fixture-proven forms. Off maps to an explicitly supported disabling form, never omission. Named choice uses the function-name object. A required control absent from that profile returns Unsupported; a reasoning model's different output-cap field needs a different tested profile revision.
Chat has separate assistant content and tool_calls channels, not arbitrary text/call interleaving. Encode only text-prefix followed by calls; concatenate adjacent text blocks, preserving bytes. Reject call→text or opaque blocks unless a dialect-specific, fixture-proven replay mapping preserves them. Canonical chat response order is text then calls by tool index, not network arrival order.
**PV:** OpenRouter reasoning-details replay needs captured raw metadata and a tested insertion rule. The initial chat profile explicitly rejects all opaque/replay-bearing blocks as Unsupported(Replay); it does not claim reasoning continuity. The old decoder emits `delta.reasoning` text but retains no replay details (`crates/ion/src/openrouter.rs:600-609`); that is not evidence of valid reasoning continuation. First live profile must avoid replay-dependent reasoning until those fixtures exist.
SSE source: `data:` payloads, `[DONE]`, indexed tool fragments and usage-only chunks are handled at `crates/ion/src/openrouter.rs:306-321,550-634`; legacy fixture examples are at `:921-958,1014-1042`. New parsing must retain finish_reason: the old decoder does not classify it (`:550-634`).
**PV:** `stop` → Completed(EndTurn), `tool_calls` → Completed(ToolCalls), `length` → Incomplete(MaxOutputTokens), `content_filter` → Incomplete(ContentFilter); an explicit refusal block → Refused. Unknown finish reason is Incomplete(Other), never success. Require a finish reason plus `[DONE]`; keep reading after finish_reason for usage. Missing `[DONE]` is Protocol even after a finish reason.
Map prompt_tokens/completion_tokens to totals; cached/cache-write detail fields to optional subsets, not invented zeroes. The old adapter subtracts cache counts and defaults missing details to zero (`crates/ion/src/openrouter.rs:565-585`); do not port that accounting convention.
Failure signals observed in source are non-2xx bodies and in-stream `{error: ...}` (`crates/ion/src/openrouter.rs:266-275,550-554`). **PV:** numeric error codes 400/401/402/403/408/429/502/503 and typed metadata drive §7; retry headers are hints, not permission for adapter retries. Capture actual bodies/headers before treating these mappings as verified.

### Responses: stateless item protocol, with Codex as source material

The second implementation targets a **public Responses endpoint profile**, not an assumption that the ChatGPT Codex backend accepts every public parameter. Legacy Codex builds top-level instructions, item input, flat function tools, `store:false`, encrypted-reasoning include and streaming (`crates/ion/src/openai_codex.rs:139-153,245-259,550-607`).
**PV:** public Responses uses `POST /v1/responses` with bearer auth; encode instructions separately, `store:false`, `stream:true`, complete input, and no previous_response_id. Use `max_output_tokens`, temperature/top_p where supported, reasoning.effort, tool_choice and parallel_tool_calls. Do not hardcode the legacy low verbosity or xhigh reasoning defaults.
User text becomes input message/input_text; assistant text becomes message/output_text; calls are separate function_call items; results are function_call_output with canonical JSON text output. Preserve item order and call_id. Function tools use top-level name/description/parameters with `strict:false`; unsupported schema keywords are refused rather than silently strengthened or removed.
On response, retain each complete output item as its block replay, including message IDs/annotations and encrypted reasoning items, and validate the neutral digest when reusing it. Opaque reasoning is not answer text. Missing required encrypted continuation data is Unsupported; do not synthesize it from displayed reasoning summaries. **PV:** exact replay eligibility and accepted returned-item fields need round-trip fixtures.
Responses can express text→call→text as distinct items. It cannot consume another provider's opaque replay, arbitrary instruction-role transitions, or unsupported content/tool types; report Unsupported. Native hosted tools, images/audio and stateful continuation are refused in this slice rather than run invisibly at the provider.
Source event names: output_text.delta, reasoning summary/text deltas, output_item.added/done, function_call_arguments.delta/done, response.completed/incomplete/failed and error (`crates/ion/src/openai_codex.rs:352-500`). Assemble by output_index and validate item/call IDs; done arguments must agree with deltas, not silently overwrite conflicts as the legacy draft path does (`:399-405,541-547`).
**PV:** final response.output is authoritative only after agreement with accumulated items; response.completed must carry status completed. Map incomplete_details.reason and usage on incomplete as well as completed; explicit refusal content → Refused, otherwise complete calls → ToolCalls and no calls → EndTurn. Unknown incomplete reasons remain Other. Require a typed terminal event; `[DONE]` alone is insufficient.
Source usage extraction reads input_tokens, output_tokens and cache details (`crates/ion/src/openai_codex.rs:609-627`). **PV:** reasoning_tokens is an output subset; capture partial/absent usage and typed error codes. response.failed/error carry failure facts, not incomplete-success; connection loss without a terminal event means Ion could not tell.
Codex-specific account/beta/originator/session headers are legacy behavior (`crates/ion/src/openai_codex.rs:266-277`), not public Responses requirements. If Codex is chosen live, require a separately named fixture-proven profile, supported output/run bounds and explicit host-supplied OAuth credentials; never copy the `originator: pi` impersonation or operation IDs.

### Shared event framing

**PV (SSE standard, not established by these legacy parsers):** decode UTF-8 incrementally; accept LF/CRLF/CR line endings, blank-line event dispatch, multiline data joined with newline, comment heartbeats and event names. Reject malformed UTF-8/JSON, conflicting event/type identifiers and incomplete trailing frames. `retry:` never activates reconnection; no Last-Event-ID resumption.
Bound error bodies, frames, blocks, indices and total response before allocation. Recognized transport-only heartbeats may be ignored; unknown semantic deltas/items fail Unsupported, malformed sequencing fails Protocol. Read non-2xx bodies under the same deadline/cancellation/byte bounds.

## 4. Conversation configuration and request assembly

Today `Conversation` has no configuration field (`crates/ion-core/src/conversation/mod.rs:14-33`); Builtins registers one ModelRef/service/catalog (`crates/ion-core/src/builtin/mod.rs:39-57`). Replace Builtins.model with the registry; retain **one host ToolCatalog**, selected per request by durable tool names. Never expose all registered tools by default.
Persist optional configuration plus its last-change CommitSeq under the conversation record, through the ordinary mutation batch/SQLite owner. Configure is a full typed replacement with expected-revision CAS; reject retired conversations, malformed settings, duplicate tool names and invalid limits. Absence is inspectable, but generation fails MissingConfiguration without dispatch.
No partial durable config: the host resolves a launch form once, displays it, and commits the complete result. Model/provider is required, instructions default empty, tools default empty, controls default to 4096 output tokens, omitted sampling/reasoning-default, Auto with tools (None without), parallel false. Unsupported explicit defaults fail; never fall back to another model.
Default run limits: 20 model steps, 3 total attempts per step, 10-minute turn deadline, 1 MiB response, 64 KiB tool output; no monetary ceiling unless explicitly supplied. Persist the turn's absolute deadline at admission, not anew at each generation. Config changes affect the next unfrozen step, but cannot increase an already-admitted turn's limits or authority.
Context defaults require an explicit model input budget (no guessed context window): compact at 80% of that budget; summary ≤ min(2048, one quarter of input budget). Include instructions, schemas and reserved output in admission estimates. Freeze estimator revision; an estimate is not proof of provider tokenization. Unknown/over-budget context blocks dispatch or runs bounded compaction first.
Instruction file discovery/reading occurs in the host outside mutation authority, then exact text is committed as configuration. Files are not reread on recovery. No layered live settings lookup: the legacy missing-file maintainer defaults and credential-bearing desktop settings are not session configuration (`crates/ion/src/settings.rs:103-114,300-328,374-389,475-489`).
Host owns credentials, permitted endpoints/proxies/TLS and immutable endpoint-profile identities. Credential rotation may change authentication only; changing account/endpoint semantics requires a new profile identity and cannot redirect a frozen attempt. The host must refuse an unavailable old profile, not repoint its label. Stricter host policy may deny dispatch, never rewrite the request.
Legacy auth resolves OpenRouter credentials and Codex credentials differently (`crates/ion/src/auth/mod.rs:184-192`; `crates/ion/src/openai_codex.rs:31-71`); neither precedence becomes an ion-ai contract. Token exchange/refresh remains host work (`crates/ion/src/auth/openrouter.rs:243-283`; `crates/ion/src/auth/codex.rs:316-338`). For R7 accept explicit environment/in-memory credentials; do not port login/storage.
Assembly obtains cut/config/placed-input bindings atomically via request_basis, then pages only through that cut outside mutation authority. Empty means empty forever. Today generation pages before capturing its last-entry cutoff (`crates/ion-core/src/builtin/generation.rs:60-61,251-262`); replace that read path, not add a second optional mode.
Fold existing head/edit/fork semantics, preserving contribution provenance and complete exchanges (`crates/ion-core/src/conversation/context/projection.rs:23-98,101-177`; `crates/ion-core/src/conversation/context/control.rs:27-35`; `crates/ion-core/src/conversation/context/fork.rs:6-11`). Restrict background model-visible injection into a busy turn; attributed inputs wait for a safe boundary. Instructions/config inheritance is explicit and independent of a history fork.
Resolve tools by sorted unique name, intersect with authority/workspace/host permission checks, and fail on an unavailable or unauthorized requested tool. Capture exact specs and implementation identities; configuration never grants execution authority. Call-time execution rechecks permissions and workspace base state even if the model saw the tool.
Built-in compaction is an ordinary task that appends a bounded summary/head at a complete exchange, then creates a new generation specification. Never truncate/rewrite a frozen request or drop replay as an adapter “context fix.” Summary attempts consume the same turn step/cost/deadline limits.

## 5. Freezing and replay

**Decision: freeze a recorded cut + exact resolved specification + digests, not a second copy of projected history.** Preserve immutable source entries and configuration evidence for the lifetime of any referencing task; archival/compaction may not delete them. Reproduction means identical neutral request and semantic wire bytes, not identical sampled output, vendor weights or network/auth headers.
Current FrozenRequest stores the full ModelRequest, cutoff, included input IDs and a counter (`crates/ion-core/src/builtin/generation.rs:232-245`); checkpoint encoding serializes that evidence and dispatch clones the request (`crates/ion-core/src/builtin/generation.rs:126-132,246-248`). This design replaces it once the wire fixtures establish the representation; there is no permanent full-request fallback.

```rust
// ion-core/src/builtin/attempt.rs; all durable, versioned serde records
pub struct ToolBinding { pub name: String, pub implementation: String, pub spec_digest: [u8; 32] }
pub struct FrozenGeneration {
    pub schema: u32, pub cut: ContextCut, pub config_revision: CommitSeq,
    pub spec: ResolvedRequestSpec, pub policy: ContextPolicy, pub limits: RunLimits,
    pub tools: Vec<ToolBinding>, pub included_inputs: Vec<InputId>,
    pub projection_revision: String, pub encoding_revision: String, pub estimator_revision: String,
    pub provider: ProviderStamp, pub request_digest: [u8; 32], pub wire_digest: [u8; 32],
}
pub enum AttemptState {
    Prepared, DispatchIntent,
    ResponseReady { response: ModelResponse, digest: [u8; 32] },
    Failed(ProviderFailure), Interrupted { usage: Usage },
}
pub struct AttemptRecord {
    pub number: u32, pub invocation: TaskInvocation, pub state: AttemptState,
    pub usage: Usage, pub next_attempt_at_ms: Option<u64>,
}
```

TaskInvocation is the existing generation/kind pair (`crates/ion-core/src/task/invocation.rs:10-13`), scoped by the enclosing task; it never crosses ion-ai. One generation checkpoint contains FrozenGeneration plus a bounded attempt vector; total attempts cannot exceed the frozen cap (hard ceiling 10). Keep turn price quote/rates and accrued/reserved/unknown cost alongside turn-root budget evidence, not in ion-ai.
Digest format `ion-request-v1`: SHA-256 over version-prefixed UTF-8 canonical JSON; recursively sort object keys, preserve array order and string bytes, use pinned serde_json number encoding, reject duplicate object keys/nonfinite numbers. Replay raw_json is an exact string in this encoding and inserted without rewriting into wire JSON. Revision covers serializer and request normalization; fixture goldens pin both digests.
Wire digest covers method, endpoint-profile revision, semantic headers and exact body bytes, not URL credentials, bearer/account secrets, dates, tracing IDs or TCP framing. Provider profile revision binds host-owned destination/account semantics without storing those secrets. No credentials or unrestricted error bodies enter checkpoints, digests or logs.
Before dispatch: assemble/project → prepare provider bytes → commit FrozenGeneration and Prepared → commit DispatchIntent with attempt number/invocation → send outside writer. A crash after DispatchIntent but before actual send is conservatively “may have run.” Reuse the prepared bytes; do not serialize again between commit and send.
On recovery, read the three-way checkpoint before consulting current configuration. Absence assembles; unreadable evidence is Indeterminate with no dispatch, continuing `crates/ion-core/src/builtin/checkpoint.rs:19-40` and `crates/ion-core/src/builtin/generation.rs:110-123`. Re-derive only context from the recorded cut with the recorded projection revision; reuse stored spec/tool bindings, not current config/files/catalog schemas.
Verify included-input provenance, request digest, provider/encoding revision and freshly prepared wire digest before any send. Later appends/heads/edits beyond the cut cannot affect it. Missing source, changed bytes or any digest mismatch produces typed ReplayMismatch and Indeterminate settlement, retaining evidence; never “repair” the checkpoint. A missing historical implementation/profile blocks recovery without dispatch so the host can restore it; no compatibility shim is required.
If ResponseReady is durable, validate its schema, response/replay invariants and digest (same versioned canonical encoding), then settle without reconstruction/networking. A process loss after complete response but before ResponseReady leaves an uncertain attempt; retry the frozen request only under §6 and record unknown usage. ResponseReady settlement atomically appends the assistant entry, admits tool children/join and records attempt accounting; cancel authority still wins if marked first.
The tool gap is explicit: Dispatch records name/call/retry_safe, not implementation (`crates/ion-core/src/builtin/tool.rs:249-255,272-278`). Add `Tool::implementation_id(&self) -> &str`; record it and spec digest in selected ToolBinding, tool task input and dispatch evidence. A same-name replacement must not execute that task; dispatched uncertainty settles Indeterminate, never-dispatched mismatch fails without execution. Default requires exact identity and both recorded/current retry-safe policy; compatibility declarations are deferred, not guessed from schema/name equality.
Initial hard caps: request 4 MiB, instructions 64 KiB, total tool specs 256 KiB, 32 calls/response, 64 KiB arguments/call, 256 blocks, 1 MiB SSE frame and response, 512 KiB replay/response. Check limits before growing buffers. Oversize rejects/terminates with LimitExceeded and usage uncertainty; no silent truncation. Larger artifacts/retention optimization belong to R6, not a parallel frozen-request store.

## 6. Cancellation, retries and recovery

Today opening the model stream is awaited outside the cancellation select; only collection races cancellation (`crates/ion-core/src/builtin/generation.rs:129-139`). Replace this with cancellation-safe ownership covering prepare/open/collect, not just token arrival.

| Cancellation point | Durable evidence and required behavior |
|---|---|
| Before connect | No intent: definitely not sent. Check cancellation before prepare and before DispatchIntent; a durable mark fences the commit. Abort settles without provider I/O. |
| During DNS/TCP/TLS/send or credential resolution | DispatchIntent remains. Select cancellation/deadline against the entire open future; dropping it ends local ownership, never proves the remote did not receive a request. |
| During stream / error-body read | Drop the owned stream/read future. Keep last durably recorded usage; missing usage stays unknown. Provisional output is discarded, not promoted to transcript. |
| After Finished, before settlement | Persist ResponseReady if still authorized. If cancellation mark wins first, normal checkpoint/settlement is fenced; fresh abort records known durable usage/result evidence but creates no tools/answer. |
| Abort invocation | Old invocation must join first. Abort never opens/retries a provider. It finalizes cancellation and any unknown attempt accounting through its restricted checkpoint/settlement authority. |

Provider guarantees cancellation-safe dropped futures/streams, no detached generation tasks and no callbacks after ownership ends; it cannot guarantee server-side cancellation or zero billing. Kernel owns durable mark, fencing, join and fresh abort generation (`DESIGN.md:408-414`). Host close is interruption, not user cancellation; reopen starts no I/O until explicit drive.
Persist failures and backoff before retry; use 1s, 2s exponential delay capped at 30s, raised to a valid Retry-After lower bound. If the hint exceeds the deadline, stop rather than shorten it. Persist the selected absolute wake time so restart does not reset waiting; use controllable clocks in tests.
Retry only RateLimited, Overloaded, transient Server, Transport or Timeout, within attempts/steps/deadline/budget. Quota/auth/permission/invalid/unsupported/safety/protocol/unknown failures are not automatic retries. ContextLength requires a new compaction/request boundary, not mutation of this attempt. Output exhaustion is incomplete, not an automatic larger-cap retry.
Generation requests use no provider-hosted side-effect tools, so retrying an uncertain generation cannot repeat an Ion tool mutation; it can duplicate billing and produce different output. Record the uncertain prior attempt before retry. If a configured cost ceiling cannot cover unresolved usage conservatively, pause/fail budget admission rather than assume zero. No claim of provider exactly-once execution.
Pre-reserve estimated worst-case tokens/cost for every dispatched attempt using the frozen model limits and host price quote revision; reconcile only reported usage. Missing rates with a configured monetary cap block admission. Without a monetary cap, retain unknown cost and enforce token/attempt/time bounds; report “cost unknown,” not a price fabricated from legacy tables.
Provisional clients receive `(TaskId, invocation identity, attachment epoch, sequence, event)` from core, never from providers. Bound the queue; coalesce/drop display deltas with an explicit gap/resnapshot signal. Durable final output replaces matching provisional frames; stale invocations cannot attach to a recovered generation.

## 7. Failure taxonomy

The current ProviderError is kind + message (`crates/ion-ai/src/error.rs:4-27`). Keep typed facts serializable and separate from process-local source chains (`error.rs`):

```rust
pub enum ProviderErrorKind {
    Authentication, Permission, InvalidRequest, ContextLength, RateLimited, Quota,
    Unsupported, Safety, Transport, Timeout, Overloaded, Server, Cancelled,
    Protocol, LimitExceeded, Unknown,
}
pub enum DispatchKnowledge { NotSent, Rejected, MayHaveRun }
pub enum RetryClass { Never, Transient }
pub struct ProviderFailure {
    pub kind: ProviderErrorKind, pub message: String,
    pub status: Option<u16>, pub code: Option<String>, pub request_id: Option<String>,
    pub unsupported: Option<UnsupportedCapability>, pub dispatch: DispatchKnowledge,
    pub retry: RetryClass, pub retry_after_ms: Option<u64>, pub usage: Usage,
}
pub enum UnsupportedCapability { Model, Instructions, Control(String), Tools, Content, Replay }
pub struct ProviderError {
    pub facts: ProviderFailure,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}
```

**PV mapping table; all rows require both APIs' fixtures where applicable.** Match structured code/type/status, never retry based on message substrings. Preserve bounded sanitized unknown codes for diagnosis.

| Signal | Neutral fact / retry |
|---|---|
| HTTP 401 / invalid_api_key; 403 / permission_denied | Authentication / Permission; Never. Host may repair credentials before a new accounted attempt, not transparently resend. |
| OpenRouter 402; insufficient_quota/billing hard-limit code (even on 429) | Quota; Never. Structured quota code overrides generic rate-limit status. |
| HTTP 429 / rate_limit_exceeded | RateLimited; Transient; parse Retry-After seconds or HTTP-date (relative to receipt clock), retaining unknown when invalid/missing. |
| HTTP 400 context_length_exceeded; Responses equivalent typed code | ContextLength; Never for the unchanged request. Generic 400 remains InvalidRequest, not inferred context length. |
| Explicit moderation/safety error; refusal block; content_filter termination | Safety failure / Refused response / Incomplete(ContentFilter), respectively; Never. Do not label every 403 as safety. |
| Explicit overloaded code or capacity 503; gateway 502/504; other 5xx | Overloaded / Server, Transient; no remote completion assumed. OpenRouter “no available provider” is overload only when fixture-classified. |
| HTTP 408; local connect/read/total deadline | Timeout; Transient unless turn deadline exhausted. Distinguish local NotSent proof from MayHaveRun. |
| DNS/TLS/socket failure, truncated stream | Transport or Protocol with MayHaveRun unless positively not sent; only Transport retries automatically. No refusal inferred. |
| Unsupported control/replay before send; malformed event/arguments; local byte cap | Unsupported / Protocol / LimitExceeded; Never; carry capability and NotSent when applicable. |
| Unrecognized provider error/status | Unknown; Never; preserve sanitized code/status, not guessed policy. |

Provider-declared refusal is positive evidence. EOF, cancellation and undecodable data mean **Ion could not tell**, with unknown completion/usage. HTTP error rejection is Rejected only when the profile proves generation did not start; in-stream errors are generally MayHaveRun. A local source chain is never serialized, logged wholesale or allowed to leak credential-bearing URLs/bodies.

## 8. Acceptance checks

All names below are proposed suites/cases, not existing green evidence. Tests must drive the new Provider trait and ordinary task driver, not only standalone JSON helpers. Fixture directories record endpoint/profile/model, capture date, provenance (synthetic vs sanitized capture) and expected request/events/error facts.

| Suite (under the named crate's `tests/`) | Required cases and assertions |
|---|---|
| ion-ai `wire_chat.rs` | `instructions_controls_tools`; `text_prefix_calls_preserve_ids`; `interleaving_is_unsupported`; `reasoning_replay_is_unsupported`; `finish_then_usage_then_done`; `length_and_filter_not_success`; `errors_and_retry_hints`. |
| ion-ai `wire_responses.rs` | `instructions_flat_tools_and_controls`; `ordered_text_call_text`; `encrypted_reasoning_item_roundtrip`; `delta_done_conflict`; `incomplete_usage_and_refusal`; `failed_and_error_events`; `foreign_replay_is_unsupported`. |
| ion-ai `sse_framing.rs` | `every_byte_split_utf8_crlf`; `multiline_comments_events`; `terminal_without_eof`; `eof_without_terminal`; `huge_index_and_frame_cap`; `unknown_semantic_event`. No adapter reconnects. |
| ion-ai `provider_contract.rs` | `unsupported_capability_surface`; `usage_unknown_not_zero`; `usage_snapshots_not_added`; `duplicate_registry_id`; `prepare_has_no_io`; `one_stream_one_http_request`; `credential_redaction`. |
| ion-core `request_assembly.rs` | `missing_and_partial_config`; `configuration_cas_and_reopen`; `instructions_survive_compaction`; `cut_captured_before_paging`; `concurrent_append_excluded`; `empty_cut_stays_empty`; `selected_tools_not_catalog`; `fork_config_is_explicit`; `background_projection_refused_while_busy`. |
| ion-core `generation_replay.rs` | `replay_mismatch_fail_closed` (mutated entry, edit, schema, opaque bytes, wire encoding); `new_config_does_not_change_recovery`; `removed_revision_blocks_recovery`; `unreadable_checkpoint_never_dispatches`; `response_ready_reopens_without_send`; `input_contribution_provenance`. |
| ion-core `provider_cancellation.rs` | `cancel_before_connect`; `cancellation_during_connect` (barrier-controlled resolver, TLS handshake and headers); `cancel_during_stream`; `cancel_after_response_before_settlement` (both writer orders); `fresh_abort_no_provider_io`; `close_reopen_unknown_attempt`. |
| ion-core `generation_limits.rs` | `retry_after_survives_restart`; `unknown_usage_blocks_cost_cap`; `deadline_spans_steps`; `step_and_attempt_exhaustion`; `compaction_safe_exchange`; `output_flood_bounded`; `stale_invocation_frames`; `fork_resume_replay`; `same_name_tool_replaced_after_reopen`. |
| headless `coding_baseline.rs` | `edit_test_disposable_workspace`: actual client submit→read→edit→exec→answer; externally assert patch and test exit, denied outside-workspace write, process timeout/output cap, no tool dispatch before final response. |

Everything above must pass **offline**: scripted providers, checked-in synthetic/captured fixtures, deterministic clock/storage barriers, disposable workspaces and loopback sockets only; no DNS to public services. Run real subprocess death around DispatchIntent/ResponseReady/tool hand-over, then explicitly reopen/drive and count externally witnessed attempts/effects.
Network-only path: proposed `cargo test -p ion-ai --features http --test live_provider -- --ignored live_provider_smoke`, gated additionally by `ION_LIVE_PROVIDER=1`, `ION_LIVE_PROVIDER_ID`, `ION_LIVE_MODEL` and explicit host credentials. Missing opt-in skips; opted-in missing configuration fails. Bound to one small tool-call/result round-trip and record latency, usage-known/unknown and sanitized wire evidence, never secrets.
Run the headless task live with `ION_LIVE_CODING=1` only in a disposable, permission-enforced workspace with frozen limits from the first mutating call. Exercise a small fixed regression set (single-file fix, test addition, failed-test repair); record exact model/config/digests, attempts, failures, wall time and cost uncertainty, with an external verifier rather than the model's success claim. At least one live provider path is required for R7; two live providers are not.
Implementations still require `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, and `cargo test --locked --workspace`. A later terminal surface additionally requires reducer/PTY checks and `scripts/smoke.sh`; headless evidence is not terminal usability evidence. No Rust or live-provider checks were run to author this design.

## 9. Idiomatic-Rust notes

Persisted contract enums use explicit tagged serde encodings and schema revisions, reject unknown required fields/variants, and have golden JSON fixtures. Replace the v0 request/message/checkpoint format directly; do not decode a missing new field as a dispatch-safe default.

Keep explicit boxed Send futures for the dyn registry, matching the existing erasure strategy (`crates/ion-ai/src/service.rs:8-16`). Native async fn in traits is useful for static dispatch but does not provide this dyn surface or imply Send futures. Do not add async-trait macros plus a second public trait. The repository pins Rust 1.98.0/edition 2024 (`rust-toolchain.toml:1-3`; `Cargo.toml:5-8`); no toolchain change is needed.
ModelStream owns transport/parser/buffers and is Send + 'static; the open future may borrow Provider, but returned streams must not. Cancellation drops the single owner; no detached producer/channel is needed. Never hold a mutex or writer guard across network await. Bound per-poll parsing work so cancellation cannot starve behind a flood.
Use thiserror for ProviderError/AssemblyError and source annotations; implement Display only where redaction requires it. No anyhow in public library contracts. Durable ProviderFailure is cloneable/serde; ProviderError is not required to be Clone, Eq or serializable. Script fixtures store facts and construct errors, not clone boxed sources.
Borrow ModelRequest/tool specs during prepare, serialize once, move PreparedRequest into stream. Move final blocks into the durable response/plan; use Arc only for genuinely shared provider/tool implementations or immutable bytes. Do not deep-clone all messages just to satisfy the stream lifetime; current generation clones the frozen request (`crates/ion-core/src/builtin/generation.rs:129-132`), which this ownership split removes.
Justified dependencies: optional reqwest with Rustls/stream and only required features, existing futures/serde/thiserror, SHA-256 for integrity, and a small bounded SSE decoder (an audited parser crate is acceptable if it exposes limits and never reconnects). Refuse vendor SDKs with opaque retry loops, general event-source reconnect clients, catalog frameworks, OAuth/keyring persistence and a second HTTP stack. Pin resolved versions in Cargo.lock and fixture-test cancellation/framing before choosing parser reuse.

## 10. Open questions

1. **Which live model/profile preserves reasoning replay and explicit bounds?** Default: OpenRouter chat without reasoning-dependent replay, plus public Responses fixtures with encrypted items. Require captured round-trips before enabling reasoning or Codex profiles. Evidence that the selected model requires opaque reasoning or rejects output controls changes the selected profile/control mapping, not the core recovery invariant.
2. **How tightly can cost admission bound uncertain billing?** Default: optional explicit monetary cap, unknown remains outstanding and blocks further spend when no conservative reservation exists; always enforce token/attempt/time limits. Captured usage/cache/reasoning totals and a dated price quote may justify tighter reservations. Subscription or variable upstream pricing must not masquerade as a known zero.
3. **Are the initial byte/token bounds usable on coding tasks?** Default: retain the stated hard caps and fail visibly; bounded response evidence is inline, history is re-derived from retained entries. Measure the small live regression set's request/output/replay sizes and recovery latency before increasing caps or introducing artifact-backed response storage. No throughput/storage claim follows from this design alone.

## 11. Deliberately out of scope, and what would have to change to bring it in

- Credential persistence/login/refresh orchestration: needs a separate host authentication design, redaction tests and explicit credential authority; never session truth.
- Dynamic catalogs, arbitrary model routing, fallback and provider breadth: require demonstrated coding-baseline needs plus two-way capability/replay fixtures; never silently substitute a frozen model.
- Images/audio, hosted provider tools, stateful response IDs and stream reconnection: require ordered-content/replay fixtures and an external side-effect/adoption contract before extending Content or retry policy.
- General instruction plugins/config layering, knowledge/memory/planners and task boards: require separate effectiveness evidence; not reasons to widen this boundary.
- Joined workers, unrestricted background projections and broad TUI/protocol work: remain behind the single-agent baseline; need explicit authority/result-selection/observation designs and their own acceptance evidence.
- Old session migration/compatibility shims: refuse/archive unsupported development schemas; preserving real sessions would need an explicit migration decision and replay-identity evidence, not a parallel production path.
