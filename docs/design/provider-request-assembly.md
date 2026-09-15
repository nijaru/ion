# Provider request assembly

Status: proposed design for review; no implementation or validation claimed. Source baseline: `aaa37fbd` (2026-09-14); Rust sources unchanged from `28b6c7a8`.
Review this core/AI surface together before implementation or trait freeze (D16, `docs/decisions.md:29`). This slice specifies the direction of `DESIGN.md` §7, §9–10, §13–14, not a new runtime. R7/R8 acceptance remains open (`ROADMAP.md:40-41`); the sequencing rationale is the holistic review (`ROADMAP.md:352`).
All Rust surfaces below are proposed replacements; signatures omit routine derives/imports and implementation bodies. Paths are repository-relative. Public API knowledge is labelled **PV**: a hypothesis requiring a versioned wire fixture, not verified vendor behavior in this repository.

## 1. Scope and non-goals

Define two boundaries: durable conversation configuration → reproducible neutral request in `ion-core`, and neutral request → one provider attempt in `ion-ai`.
Observable result: a headless conversation sends its selected instructions, model, controls, tools and ordered context; recovery sends the same request or refuses before networking. Providers cannot execute tools or mutate sessions.
Use OpenRouter's chat-completions dialect and Anthropic Messages, following accepted D15 (`docs/decisions.md:28`). Start live testing with one explicitly selected provider/model, not a catalog.
No speculative configuration framework, provider catalog breadth, credential storage, routing policy engine, alternate runtime, automatic cross-provider fallback or generic Effect lifecycle.
The active workspace excludes the legacy application (`Cargo.toml:1-3`); its adapters are source material, not working integrations. Selectively rebuild wire leaves, never restore their operation-bound interfaces (`crates/ion/src/openrouter.rs:21-24`; `crates/ion/src/openai_codex.rs:16-19`).
Execution enforcement and a terminal UI are separate boundaries. This document specifies the inputs/evidence they must supply and the joint acceptance task, not their implementations.

## 2. Contract

### `ion-ai`: request and ordered content

Keep `ModelRef { provider: String, model: String }` and `ToolSpec { name, description, input_schema: Value }` (`crates/ion-ai/src/model.rs:3-7`; `crates/ion-ai/src/tool.rs:4-9`). Provider names identify host registrations, not wire API families.
Replace the current three-field request (`crates/ion-ai/src/request.rs:5-10`) with the following. `ResolvedRequestSpec` lives in `request.rs`; controls in `controls.rs`; Message/Role/ProviderReplay in `message.rs`, and ContentBlock/Content/ToolCall/ToolResult in `content.rs`; `lib.rs` re-exports the surface.

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
pub enum Reasoning { ProviderDefault, Off, Low, Medium, High, BudgetTokens(u32) }
pub enum ToolChoice { None, Auto, Required, Named(String) }
pub enum Role { User, Assistant, Tool }
pub struct Message { pub role: Role, pub content: Vec<ContentBlock> }
pub struct ContentBlock { pub content: Content, pub replay: Option<ProviderReplay> }
pub enum Content {
    Text(String), ToolCall(ToolCall), ToolResult(ToolResult),
    PartialToolCall { id: String, name: String, arguments_json: String },
    Refusal(String), Opaque { summary: Option<String> },
}
pub struct ToolCall { pub id: String, pub name: String, pub arguments: Value }
pub struct ToolResult { pub call_id: String, pub name: String, pub result: Value, pub is_error: bool }
pub struct ProviderReplay {
    pub origin: ModelRef,
    pub api: String,             // e.g. "anthropic-messages-v1"
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
For Anthropic, neutral blocks retain content-array order. Thinking/redacted-thinking blocks are Opaque with exact continuation fields in replay; displayed summary text is not a substitute. Unknown semantic block types return Unsupported, not fabricated text. Tool errors/cancelled/indeterminate results set is_error and retain their actual structured evidence in result.
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
`Finished` occurs exactly once and ends the stream; its blocks must agree with ended blocks and accumulated deltas. Final replay may arrive only at BlockEnd. `Err` is terminal; EOF without Finished is a protocol failure, not evidence of provider completion. Partial calls never execute, even if one block ended. On explicit incomplete/refused termination, an unfinished argument buffer ends as output-only PartialToolCall; defer that end until the stop reason is known. Requests reject this variant. Invalid JSON under claimed Completed remains Protocol, never a repaired call.
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

`prepare` is pure: validate model, instructions, every explicit control, tool schema, ordering and replay; serialize the complete semantic request with no credential reads or networking. `ProviderRegistry` wraps every stream with the shared validator (§10.4) before core can observe it, so an implementation cannot admit an out-of-order or incomplete response even if it validates nothing itself. Unsupported capabilities are typed failures here, not a list of optimistic booleans. Reject duplicate/empty registry IDs; unknown provider/model is Unsupported. No registration replacement while invocations use that registry. Provider/model profiles are a finite host-supplied table; no discovery request runs during assembly.
PreparedRequest::new validates byte caps and an allowlist of nonsecret semantic headers, computes the digest, and exposes no mutation. Method is POST; route is fixed by the endpoint-profile revision. Prepared bytes are immutable and movable, tied to their originating provider stamp. `stream` verifies ownership, resolves host credentials, and makes **at most one generation HTTP request**. No redirects, reconnects, provider fallback or hidden SDK retries. Neither a request nor a stamp contains Ion session/task IDs.
Host instances live in optional `crates/ion-ai/src/provider/{openrouter,anthropic}.rs`; shared wire leaves in `crates/ion-ai/src/api/{chat,anthropic}/{request,stream,error}.rs` and `api/sse.rs`. Enable HTTP behind one feature, leaving contract/scripted builds HTTP-free. Do not pre-create auth/catalog abstractions.

### `ion-core`: durable configuration and assembly surfaces

Within `crates/ion-core/src/`, `conversation/config.rs` owns configuration; `builtin/request.rs` owns assembly; `builtin/attempt.rs` owns frozen/attempt evidence. `session/config.rs` owns writer validation/CAS. These are proposed additions, not general extension points.

```rust
pub struct ConversationConfig {
    pub model: ModelRef, pub instructions: String, pub controls: GenerationControls,
    pub project_context: Vec<Message>, pub instruction_revision: String,
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

PV verification targets: [OpenRouter requests](https://openrouter.ai/docs/api/reference/overview), [OpenRouter streaming](https://openrouter.ai/docs/api/reference/streaming), [Anthropic Messages](https://platform.claude.com/docs/en/api/messages/create), [Anthropic streaming](https://platform.claude.com/docs/en/build-with-claude/streaming), and [SSE framing](https://html.spec.whatwg.org/multipage/server-sent-events.html). Anthropic streaming event/delta documentation was fetched and inspected on 2026-09-14; it remains vendor description, not an Ion wire fixture. Other links are verification targets.

### Chat completions: OpenRouter profile

Source evidence: legacy request/tools/instructions are encoded in `crates/ion/src/openrouter.rs:99-113,220-236,460-506`; bearer auth and cancellable send at `crates/ion/src/openrouter.rs:239-275`. It sends `reasoning.effort`, rather than the generic OpenAI `reasoning_effort` spelling (PV). This design gives that dialect its own profile; “OpenAI-compatible” does not imply identical controls.
Encode instructions as the first system message (including an explicitly empty string), then ordered messages. Tool specs become `tools[].function.{name,description,parameters}`; calls retain provider IDs and encode arguments as canonical JSON strings; tool results use `tool_call_id` and canonical JSON `{is_error,result}` text as content. This versioned text envelope exposes the neutral error flag that Chat lacks, rather than silently losing it.
**PV:** send `max_tokens`, temperature, top_p, tool_choice and parallel_tool_calls only using the selected model/profile's fixture-proven forms. Off maps to an explicitly supported disabling form, never omission. Named choice uses the function-name object. A required control absent from that profile returns Unsupported; a reasoning model's different output-cap field needs a different tested profile revision. BudgetTokens is Unsupported in this first chat profile; do not translate token budgets into qualitative effort.
Chat has separate assistant content and tool_calls channels, not arbitrary text/call interleaving. Encode only text-prefix followed by calls; concatenate adjacent text blocks, preserving bytes. Reject call→text or opaque blocks unless a dialect-specific, fixture-proven replay mapping preserves them. Canonical chat response order is text then calls by tool index, not network arrival order.
**PV:** OpenRouter reasoning-details replay needs captured raw metadata and a tested insertion rule. The initial chat profile explicitly rejects all opaque/replay-bearing blocks as Unsupported(Replay); it does not claim reasoning continuity. The old decoder emits `delta.reasoning` text but retains no replay details (`crates/ion/src/openrouter.rs:600-609`); that is not evidence of valid reasoning continuation. First live profile must avoid replay-dependent reasoning until those fixtures exist.
SSE source: `data:` payloads, `[DONE]`, indexed tool fragments and usage-only chunks are handled at `crates/ion/src/openrouter.rs:306-321,550-634`; legacy fixture examples are at `crates/ion/src/openrouter.rs:921-958,1014-1042`. New parsing must retain finish_reason: the old decoder does not classify it (`crates/ion/src/openrouter.rs:550-634`).
**PV:** `stop` → Completed(EndTurn), `tool_calls` → Completed(ToolCalls), `length` → Incomplete(MaxOutputTokens), `content_filter` → Incomplete(ContentFilter); an explicit refusal block → Refused. Unknown finish reason is Incomplete(Other), never success. Require a finish reason plus `[DONE]`; keep reading after finish_reason for usage. Missing `[DONE]` is Protocol even after a finish reason.
Map prompt_tokens/completion_tokens to totals; cached/cache-write detail fields to optional subsets, not invented zeroes. The old adapter subtracts cache counts and defaults missing details to zero (`crates/ion/src/openrouter.rs:565-585`); do not port that accounting convention.
Failure signals observed in source are non-2xx bodies and in-stream `{error: ...}` (`crates/ion/src/openrouter.rs:266-275,550-554`). **PV:** numeric error codes 400/401/402/403/408/429/502/503 and typed metadata drive §7; retry headers are hints, not permission for adapter retries. Capture actual bodies/headers before treating these mappings as verified.

### Anthropic Messages: top-level system, native content blocks and thinking replay

**PV throughout this mapping:** Anthropic behavior is specified from public API knowledge, not claimed as implemented by the inspected legacy adapters. Vendor descriptions and synthetic fixtures must be distinguished from sanitized captures; D15 chooses this API precisely to expose differences rather than infer neutrality from two similar adapters.
Send `POST /v1/messages`, `x-api-key` from host credentials, a profile-pinned `anthropic-version`, `stream:true`, model and required max_tokens. Map instructions to top-level `system` text, never a system-role message. Omit an empty system string by this profile's fixed encoding rule. Function tools use `{name,description,input_schema}`, without Chat's function wrapper.
Map temperature/top_p directly only on fixture-proven models; reasoning Off → `thinking:{type:"disabled"}`, ProviderDefault → omit. BudgetTokens(n) → `thinking:{type:"enabled",budget_tokens:n}` with positive n below max_tokens and the profile's minimum. Low/Medium/High require a tested adaptive-thinking model/profile and map to `thinking:{type:"adaptive"}` plus `output_config.effort`; never guess a token budget from an effort label. Unsupported combinations (including forced tools with some thinking modes) fail before networking.
ToolChoice None/Auto/Required/Named maps to `{type:"none"}`, `{type:"auto"}`, `{type:"any"}`, `{type:"tool",name}` respectively; parallel false adds `disable_parallel_tool_use:true` where tools are enabled. With no tools, omit tool_choice/parallel fields because the constraint is vacuous. Never silently omit a nonvacuous requested constraint.
User text stays user content; assistant Text/ToolCall become ordered text/tool_use blocks (`id,name,input`). Adjacent core tool-result messages become one user message containing tool_result blocks in originating-call order (`tool_use_id,content,is_error`); content is canonical JSON result text. Group adjacent same-role messages without moving content across blocks or inventing conversation text.
Anthropic preserves text→tool_use→text and thinking positions within one content array. It has no independent tool role or arbitrary system messages inside history; those are mapped only by the explicit grouping/top-level rule above. Unknown block types, hosted tools, media, citations requiring unsupported replay, cross-provider replay and untested controls return Unsupported.
Thinking blocks retain complete thinking text and signature; redacted_thinking retains exact data. Store the complete block in ProviderReplay at that ordered Opaque position, validate origin/API/revision/content digest, then reinsert the block unmodified. An edited/missing signature or stripped thinking needed by an exchange is Unsupported(Replay). No provider switch, compaction or adapter default may silently drop it.
For streaming replay, accumulate thinking_delta and signature_delta string fragments exactly, then serialize their assembled block envelope once; that stored JSON is immutable thereafter. This preserves opaque field values, not nonexistent original whole-block whitespace. When a complete block object is supplied, retain its raw JSON. Tool input_json_delta fragments concatenate until content_block_stop; parse once as an object, preserving the provider call ID, without an empty-argument fallback.
Framing/events: message_start establishes response ID and initial usage; content_block_start/delta/stop map to BlockStart/Delta/End by index; text_delta → Text, input_json_delta.partial_json → ArgumentsJson, thinking_delta → provisional Summary, signature_delta stays internal until BlockEnd. ping is transport-only. message_delta supplies stop_reason and cumulative usage; message_stop terminates. A stop reason without message_stop, or message_stop with unfinished blocks/no stop reason, is Protocol.
Map end_turn → Completed(EndTurn), tool_use → Completed(ToolCalls), max_tokens → Incomplete(MaxOutputTokens), model_context_window_exceeded → Incomplete(ContextLength), refusal → Refused. stop_sequence is Incomplete(Other) with a diagnostic because this contract sent no stop sequences; pause_turn/unknown reasons stay Incomplete(Other), never successful final answers. Refusal and incomplete usage are retained, with no tool execution.
Usage input_tokens excludes cache buckets in the documented Anthropic convention: neutral input total is input_tokens + cache_creation_input_tokens + cache_read_input_tokens **only when every component is known**. Preserve cache counts as subsets and output_tokens as total; absent component → unknown total unless the exact profile fixture proves omission means zero. Do not infer a reasoning-token split. Usage at message_delta updates, not adds to, message_start usage.
Errors can arrive as HTTP non-2xx JSON or SSE `event:error` with `{type:"error",error:{type,message}}` after HTTP 200. Map rate_limit_error/429, overloaded_error/529, authentication_error/401, permission_error/403, invalid_request_error/400, request_too_large/413 and api_error/500 through §7. Retry-After is advisory. Error after partial output remains MayHaveRun; neither message_stop nor billing-free rejection is inferred.
Legacy Codex remains useful negative evidence: it puts instructions above input, requests encrypted reasoning and emits a different set of terminal events (`crates/ion/src/openai_codex.rs:245-259,352-500`), but its input conversion does not replay those encrypted items (`crates/ion/src/openai_codex.rs:550-607`). Its account/beta/originator headers and string-only failure extraction are not portable contracts (`crates/ion/src/openai_codex.rs:266-277,630-654`). Do not port these limitations or add a third adapter in this slice.

### Shared event framing

**PV (SSE standard, not established by these legacy parsers):** decode UTF-8 incrementally; accept LF/CRLF/CR line endings, blank-line event dispatch, multiline data joined with newline, comment heartbeats and event names. Reject malformed UTF-8/JSON, conflicting event/type identifiers and incomplete trailing frames. `retry:` never activates reconnection; no Last-Event-ID resumption.
Bound error bodies, frames, blocks, indices and total response before allocation. Recognized transport-only heartbeats may be ignored; unknown semantic deltas/items fail Unsupported, malformed sequencing fails Protocol. Read non-2xx bodies under the same deadline/cancellation/byte bounds.

## 4. Conversation configuration and request assembly

Today `Conversation` has no configuration field (`crates/ion-core/src/conversation/mod.rs:14-33`); Builtins registers one ModelRef/service/catalog (`crates/ion-core/src/builtin/mod.rs:39-57`). Replace Builtins.model with the registry; retain **one host ToolCatalog**, selected per request by durable tool names. Never expose all registered tools by default.
Persist optional configuration plus its last-change CommitSeq under the conversation record, through the ordinary mutation batch/SQLite owner. Configure is a full typed replacement with expected-revision CAS; reject retired conversations, malformed settings, duplicate tool names and invalid limits. Absence is inspectable, but generation fails MissingConfiguration without dispatch.
No partial durable config: the host resolves a launch form once, displays it, and commits the complete result. Model/provider is required, instructions/project context default empty, instruction revision defaults to `ion-instructions-v1`, tools default empty, controls default to 4096 output tokens, omitted sampling/reasoning-default, Auto with tools (None without), parallel false. Unsupported explicit defaults fail; never fall back to another model.
Default request-byte cap is 4 MiB. Default run limits: 20 model steps, 3 total attempts per step, 10-minute turn deadline, 1 MiB response, 64 KiB tool output; no monetary ceiling unless explicitly supplied. Persist the turn's absolute deadline at admission, not anew at each generation. Config changes affect the next unfrozen step, but cannot increase an already-admitted turn's limits or authority.
Context defaults require an explicit model input budget (no guessed context window): compact at 80% of that budget; summary ≤ min(2048, one quarter of input budget). Include instructions, schemas and reserved output in admission estimates. Freeze estimator revision; an estimate is not proof of provider tokenization. Unknown/over-budget context blocks dispatch or runs bounded compaction first.
Instruction/project-context discovery and reading occur in the host outside mutation authority, then exact instruction text and bounded project-context messages are committed as configuration. project_context accepts only user Text blocks (not tool exchanges or elevated instructions); its ordered prefix precedes transcript messages. Record the host selection/ordering algorithm revision in instruction_revision. This freezes resolved content, not paths to reread, satisfying tightened D13 (`docs/decisions.md:25`). Files are not reread on recovery. No layered live settings lookup: the legacy missing-file maintainer defaults and credential-bearing desktop settings are not session configuration (`crates/ion/src/settings.rs:103-114,300-328,374-389,475-489`).
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
    pub project_context: Vec<Message>, pub instruction_revision: String,
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
On recovery, read the three-way checkpoint before consulting current configuration. Absence assembles; unreadable evidence is Indeterminate with no dispatch, continuing `crates/ion-core/src/builtin/checkpoint.rs:19-40` and `crates/ion-core/src/builtin/generation.rs:110-123`. Re-derive only transcript context from the recorded cut with the recorded projection revision; prepend the stored project_context, and reuse instruction_revision and spec.instructions unchanged; reuse stored spec/tool bindings, not current config/files/catalog schemas.
Verify included-input provenance, request digest, recorded instruction/projection/estimator/encoding revisions and provider stamp and freshly prepared wire digest before any send. Later appends/heads/edits beyond the cut cannot affect it. Missing source, changed bytes or any digest mismatch produces typed ReplayMismatch and Indeterminate settlement, retaining evidence; never “repair” the checkpoint. A missing historical implementation/profile blocks recovery without dispatch so the host can restore it; no compatibility shim is required.
If ResponseReady is durable, validate its schema, response/replay invariants and digest (same versioned canonical encoding), then settle without reconstruction/networking. A process loss after complete response but before ResponseReady leaves an uncertain attempt; retry the frozen request only under §6 and record unknown usage. ResponseReady settlement atomically appends the assistant entry, admits tool children/join and records attempt accounting; cancel authority still wins if marked first.
The tool gap is explicit: Dispatch records name/call/retry_safe, not implementation (`crates/ion-core/src/builtin/tool.rs:249-255,272-278`). Add `Tool::implementation_id(&self) -> &str`; record it and spec digest in selected ToolBinding, tool task input and dispatch evidence. A same-name replacement must not execute that task; dispatched uncertainty settles Indeterminate, never-dispatched mismatch fails without execution. Default requires exact identity and both recorded/current retry-safe policy; compatibility declarations are deferred, not guessed from schema/name equality.
Initial hard caps: request 4 MiB, instructions 64 KiB, project context 128 KiB, total tool specs 256 KiB, 32 calls/response, 64 KiB arguments/call, 256 blocks, 1 MiB SSE frame and response, 512 KiB replay/response, 64 KiB error body and 4 KiB sanitized failure message. Check limits before growing buffers. Oversize rejects/terminates with LimitExceeded and usage uncertainty; no silent truncation. Larger artifacts/retention optimization belong to R6, not a parallel frozen-request store. Cut-based freezing removes durable history duplication; it does not establish bounded cold-projection work or session residency, which still need R6 measurements.

## 6. Cancellation, retries and recovery

Today opening the model stream is awaited outside the cancellation select; only collection races cancellation (`crates/ion-core/src/builtin/generation.rs:129-139`). Replace this with cancellation-safe ownership covering prepare/open/collect, not just token arrival.

| Cancellation point | Durable evidence and required behavior |
|---|---|
| Before connect | No intent: definitely not sent. Check cancellation before prepare and before DispatchIntent; a durable mark fences the commit. Abort settles without provider I/O. |
| During DNS/TCP/TLS/send or credential resolution | DispatchIntent remains. Select cancellation/deadline against the entire open future; dropping it ends local ownership, never proves the remote did not receive a request. |
| During stream / error-body read | Drop the owned stream/read future. Keep last durably recorded usage; missing usage stays unknown. Provisional output is discarded, not promoted to transcript. |
| After Finished, before settlement | Persist ResponseReady if still authorized. If cancellation mark wins first, normal checkpoint/settlement is fenced; fresh abort records known durable usage/result evidence but creates no tools/answer. |
| Abort invocation | Old invocation must join first. Abort never opens/retries a provider. It finalizes cancellation and any unknown attempt accounting through its restricted checkpoint/settlement authority. |

Provider guarantees cancellation-safe dropped futures/streams, no detached generation tasks and no callbacks after ownership ends; it cannot guarantee server-side cancellation or zero billing. Kernel owns durable mark, fencing, join and fresh abort generation (`DESIGN.md:413-421`). Host close is interruption, not user cancellation; reopen starts no I/O until explicit drive.
Persist failures and backoff before retry; use 1s, 2s exponential delay capped at 30s, raised to a valid Retry-After lower bound. If the hint exceeds the deadline, stop rather than shorten it. Persist the selected absolute wake time so restart does not reset waiting; use controllable clocks in tests.
Retry only RateLimited, Overloaded, transient Server, Transport or Timeout, within attempts/steps/deadline/budget. Quota/auth/permission/invalid/unsupported/safety/protocol/unknown failures are not automatic retries. ContextLength requires a new compaction/request boundary, not mutation of this attempt. Output exhaustion is incomplete, not an automatic larger-cap retry. Exhausted known rejections/refusals/incomplete responses settle Failed with typed evidence; an exhausted uncertain dispatch with no durable final response settles Indeterminate, not a known application failure.
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
/// Facts only. Retry eligibility is generation policy (§10.3): the adapter reports
/// what happened and an advisory delay, never whether another attempt is allowed.
pub struct ProviderFailure {
    pub kind: ProviderErrorKind, pub message: String,
    pub status: Option<u16>, pub code: Option<String>, pub request_id: Option<String>,
    pub unsupported: Option<UnsupportedCapability>, pub dispatch: DispatchKnowledge,
    pub retry_after_ms: Option<u64>, pub usage: Usage,
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
| HTTP 401 / invalid_api_key / authentication_error; 403 / permission_denied / permission_error | Authentication / Permission; Never. Host may repair credentials before a new accounted attempt, not transparently resend. |
| OpenRouter 402; insufficient_quota/billing hard-limit code (even on 429) | Quota; Never. Structured quota code overrides generic rate-limit status. |
| HTTP 429 / rate_limit_exceeded / rate_limit_error | RateLimited; Transient; parse Retry-After seconds or HTTP-date (relative to receipt clock), retaining unknown when invalid/missing. |
| HTTP 400 context_length_exceeded; Anthropic explicit context-limit code when fixture-proven | ContextLength; Never for the unchanged request. Generic invalid_request_error/400 remains InvalidRequest, not inferred context length; request_too_large/413 is a payload-limit failure, not a token-count claim. |
| Explicit moderation/safety error; refusal block; content_filter termination | Safety failure / Refused response / Incomplete(ContentFilter), respectively; Never. Do not label every 403 as safety. |
| Anthropic overloaded_error/529 or capacity 503; gateway 502/504; api_error/other 5xx | Overloaded / Server, Transient; no remote completion assumed. OpenRouter “no available provider” is overload only when fixture-classified. |
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
| ion-ai `wire_anthropic.rs` | `top_level_system_native_tools_and_controls`; `ordered_text_call_text`; `thinking_signature_and_redacted_roundtrip`; `tool_result_user_grouping`; `input_json_fragments`; `usage_cache_totals_partial_and_missing`; `incomplete_and_refusal`; `max_tokens_mid_tool_json`; `overloaded_error_after_200`; `foreign_replay_is_unsupported`. |
| ion-ai `sse_framing.rs` | `every_byte_split_utf8_crlf`; `multiline_comments_events`; `terminal_without_eof`; `eof_without_terminal`; `huge_index_and_frame_cap`; `unknown_semantic_event`. No adapter reconnects. |
| ion-ai `provider_contract.rs` | `unsupported_capability_surface`; `usage_unknown_not_zero`; `usage_snapshots_not_added`; `duplicate_registry_id`; `prepare_has_no_io`; `one_stream_one_http_request`; `credential_redaction`. |
| ion-core `request_assembly.rs` | `missing_and_partial_config`; `configuration_cas_and_reopen`; `instructions_survive_compaction`; `project_files_change_after_freeze`; `instruction_projection_bytes_are_frozen`; `cut_captured_before_paging`; `concurrent_append_excluded`; `empty_cut_stays_empty`; `selected_tools_not_catalog`; `fork_config_is_explicit`; `background_projection_refused_while_busy`. |
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

## 10. Resolved review gates

Source/design audit at `1d370462` (2026-09-14) kept the ownership direction and refused a trait freeze until four gaps had concrete contracts. They are decided here; implementation is still staged (§10.5). Wire mappings remain PV hypotheses until their fixtures exist.

### 10.1 One authoritative turn budget (`D19`)

The budget is **durable state on the turn root task record**, not a new entity and not a copy per generation:

```rust
// tasks row, schema v6; present only on a turn root
pub struct TurnBudget {
    pub limits: RunLimits,               // frozen at turn admission
    pub deadline_at_ms: u64,             // absolute, from admission
    pub reserved: BudgetCharge,          // worst case for attempts in flight
    pub settled: BudgetCharge,           // reconciled, reported usage
    pub unknown_attempts: u32,           // dispatched attempts with no usable usage
}
pub struct BudgetCharge {
    pub input_tokens: u64, pub output_tokens: u64,
    pub cost_microusd: Option<u64>,      // None only when no monetary cap is configured
}
```

Writer commands, both validated against the live generation-fencing rules:

```text
reserve_turn_budget(turn: TaskId, generation: u64, charge: BudgetCharge) -> BudgetReservation
reconcile_turn_budget(turn: TaskId, reservation: BudgetReservation, usage: Usage) -> BudgetCharge
```

The turn root stays the addressing key because every member already carries `task.turn`, so a continuation, a compaction task and a tool task all reach the same balance without a second index. Rules:

- Limits and the absolute deadline are frozen when the turn opens. Config changes and authority narrowing affect the next turn, never the live one; nothing may raise a live budget.
- A generation, compaction or any future budgeted kind reserves **before** its dispatch-intent commit, so a crash after reservation leaves a conservative charge. Reuse of the reservation across a retry is an explicit choice, not a default.
- Reconciliation converts a reservation into `settled`, or into `unknown_attempts` plus an unresolved charge when usage is unknown. `Usage`'s unknown-vs-zero distinction is preserved end to end: an unknown attempt never reconciles to zero.
- With a monetary cap configured, an unknown unresolved amount blocks further admission rather than being assumed free; token/attempt/time limits are enforced regardless.
- A per-generation checkpoint keeps its own reservation record as *evidence* of what that attempt claimed. Only the turn budget authorizes spend.

Acceptance (`ion-core generation_limits.rs`): two concurrent reservations cannot both fit a cap; crash after reserve still charges the attempt; unknown usage with a cost cap blocks the next step; a tightened configuration does not change the live turn; a compaction attempt charges the same budget; reserving after the deadline fails without dispatch.

### 10.2 Recoverably blocked replay (`D20`)

A missing historical revision, provider, profile or tool implementation is **host state that can be restored**, not corrupt evidence, so it must not terminalize work:

```rust
// ion-ai: exchanged between assembly and the provider boundary
pub enum Unavailable { Provider(String), Profile(String), Model(ModelRef), ToolImplementation(String), Revision(String) }

// ion-core: a drive that cannot proceed
pub enum DriveOutcome { Settled(..), Interrupted(..), Blocked { task_id: TaskId, reason: Unavailable } }
```

`Blocked` consumes no invocation generation, writes no outcome, leaves the task `Running` with its checkpoint intact, and performs no provider or tool I/O. An explicit re-drive after the host registers the missing item proceeds exactly once; a still-missing item blocks again. This is the provider-side analogue of the already-accepted rule that a running task whose `(kind, schema_version)` implementation is unavailable blocks recovery.

Contrast, and the reason both exist: a checkpoint that exists but cannot be read stays terminal `Indeterminate` with no dispatch (`crates/ion-core/src/builtin/checkpoint.rs`). Unreadable evidence is never "blocked", and blocked work is never settled.

Acceptance (`ion-core generation_replay.rs`): close with a missing profile, reopen, drive → `Blocked`, generation unchanged, no send; register the profile, drive → sends once; the same sequence with a damaged checkpoint settles `Indeterminate` instead.

### 10.3 Provider reports facts; generation owns retry policy (`D20`)

`ProviderFailure` carries `kind`, `message`, `status`, `code`, `request_id`, `unsupported`, `dispatch`, `retry_after_ms` and `usage`. **`RetryClass` is removed**: it duplicated the eligibility rule and could disagree with §6's kind-based table. The adapter may classify what happened (`dispatch`, advisory timing, structural code); only the generation task decides whether another attempt is allowed, from the typed facts plus attempts/steps/deadline/budget. `retry_after_ms` is timing input for a backoff the generation chooses, never permission to retry. Contradictory input (a structural retry hint on a non-retryable kind) resolves to the kind, and unknown failures never retry automatically.

Acceptance (`ion-core generation_limits.rs`, `ion-ai provider_contract.rs`): authentication and invalid-request failures do not retry despite retry hints; a rate-limited failure retries with the advisory delay; an unknown kind does not; policy decisions are asserted from enum facts, not message text.

### 10.4 One shared stream validator at the trait boundary (`D20`)

Provisional-block ordering cannot be enforced by convention, so every stream reaching core passes one validator:

```rust
// ion-ai: applied by the registry wrapper, not trusted to each implementation
pub fn validate_stream(stream: ModelStream) -> ModelStream;
```

It enforces: unique `BlockStart` per index, deltas only for an open block, one `BlockEnd` per started index, exactly one terminal `Finished`, `Finished`/`BlockEnd` blocks agreeing with accumulated deltas, indices and buffers within bounds, and no event after termination. A violation terminates the stream with `ProviderErrorKind::Protocol` and `DispatchKnowledge::MayHaveRun`. Wire parsers additionally validate their own framing (§3); a scripted or third-party provider gets the same treatment because the wrapper is applied by `ProviderRegistry`/`ModelService`, not by the adapter.

Acceptance (`ion-ai provider_contract.rs`, `ion-core generation_replay.rs`): malformed final blocks, duplicate indices, deltas without a start, a second `Finished` and post-terminal events each fail as `Protocol` through the real driver, with no assistant entry and no tool child.

### 10.5 Delivery stages

Resolved contracts do not authorize a big-bang replacement. Deliver in bounded stages, each with its own gates and evidence:

1. **Neutral types, validator and atomic request basis** — no networking; scripted provider only. The first code slice is the request basis (§4: capture cut/config/placed inputs atomically, with an upper cutoff), because it is independent of the wire work and closes the "cutoff captured after paging" finding.
2. **Two wire fixtures and one bounded live path** (`ION_LIVE_PROVIDER=1`, one explicitly selected provider/model).
3. **Enforced budgets and the execution environment** before any mutating live run.

Do not turn the complete R7/R8 acceptance matrix into a prerequisite for the first offline fixture.

## 11. Open questions

1. **Which live model/profile preserves reasoning replay and explicit bounds?** Default: OpenRouter chat without reasoning-dependent replay, plus Anthropic fixtures with signed/redacted thinking blocks. Require captured round-trips before enabling model-specific reasoning controls or switching the live path to Anthropic. Evidence that the selected model requires opaque reasoning or rejects output controls changes the selected profile/control mapping, not the core recovery invariant.
2. **How tightly can cost admission bound uncertain billing?** Default: optional explicit monetary cap, unknown remains outstanding and blocks further spend when no conservative reservation exists; always enforce token/attempt/time limits. Captured usage/cache/reasoning totals and a dated price quote may justify tighter reservations. Subscription or variable upstream pricing must not masquerade as a known zero.
3. **Are the initial byte/token bounds usable on coding tasks?** Default: retain the stated hard caps and fail visibly; bounded response evidence is inline, history is re-derived from retained entries. Measure the small live regression set's request/output/replay sizes and recovery latency before increasing caps or introducing artifact-backed response storage. No throughput/storage claim follows from this design alone.

## 12. Deliberately out of scope, and what would have to change to bring it in

- Credential persistence/login/refresh orchestration: needs a separate host authentication design, redaction tests and explicit credential authority; never session truth.
- Dynamic catalogs, arbitrary model routing, fallback and provider breadth: require demonstrated coding-baseline needs plus two-way capability/replay fixtures; never silently substitute a frozen model.
- Images/audio, hosted provider tools, stateful response IDs and stream reconnection: require ordered-content/replay fixtures and an external side-effect/adoption contract before extending Content or retry policy.
- General instruction plugins/config layering, knowledge/memory/planners and task boards: require separate effectiveness evidence; not reasons to widen this boundary.
- Joined workers, unrestricted background projections and broad TUI/protocol work: remain behind the single-agent baseline; need explicit authority/result-selection/observation designs and their own acceptance evidence.
- Old session migration/compatibility shims: refuse/archive unsupported development schemas; preserving real sessions would need an explicit migration decision and replay-identity evidence, not a parallel production path.
