# Pico core and AI-boundary review — 2026-09-12

Scope: the current `earendil-works/pi` `pico` branch and `pi-ai`, used as evidence for Ion's core rewrite and the later model/provider subsystem pass.

This review is intentionally not a parity plan. Pico is a strong minimal-harness reference; Ion should copy only ideas that survive its Rust, durability, local-first, multi-conversation session, and recovery requirements.

## Historical revision and refresh

This review describes the pinned September 12 revision, not current upstream.
A subsequent source check at `b02eef418672317f30247093a8e68797b8cfe144`
found the original `harness/pico/` removed, a retained `harness/pico3/`
implementation, and a normative but explicitly unimplemented Pico5 design.
See upstream [`pico-v5-handoff.md`](https://github.com/earendil-works/pi/blob/b02eef418672317f30247093a8e68797b8cfe144/packages/agent/docs/pico-v5-handoff.md)
for that distinction. The findings below remain historical rationale; recheck
current implementation before using them to describe Pico's capabilities.
Upstream redesigns do not reopen Ion's accepted kernel contracts by themselves.

## Pico authority at the reviewed revision

At `earendil-works/pi` `pico` branch head `7a2647f32a11864d0c2f98bd2278d18fdf524f9a`, `packages/agent/docs/pico/pico-simple-handoff.md` declares itself the sole normative implementation specification for the clean-room Pico harness. Older Pico/Pico-v3 documents are historical inputs.

The implementation currently present under `packages/agent/src/harness/pico/` contains the foundation modules (`addresses`, `entries`, `runtime`, `tasks`, etc.). The normative specification explicitly gates provider generation/system integration, complete built-in tool schemas, some hooks, jobs/subagents, and renderer/client integration rather than guessing unfinished interfaces.

That is useful evidence for Ion: settle the generic kernel before coupling provider/tool/client code to it.

## Core shape

Pico's foundation is very small:

```text
Session
  Conversations
    immutable Entries
    durable Tasks
    scoped State
```

One process owns a session. Tasks/effects execute concurrently; every canonical mutation and scheduling decision serializes through one commit line.

### Conversation is the agent-thread primitive

A conversation contains:

- an ID;
- optional history parent + cutoff;
- optional owning task.

A subagent is an owned conversation, not a distinct durable `Agent` record and not a special subagent task. History parentage and execution ownership are independent edges.

This supports Ion's current move to `Session + Conversation + Task` rather than `Session + Agent + Conversation`.

## Context representation

Current Pico does **not** persist a mutable context list. Entries are immutable and may materialize generic facets:

- `data`: typed/kind-specific durable data;
- `model`: provider-neutral model-message projection;
- `head`: a new retained-context boundary;
- `edits`: omission/replacement controls targeting earlier projected entries.

Context is derived by finding the newest visible head, reading the fork-visible range, folding edits, concatenating stored projections, and normalizing tool exchanges.

Advantages relevant to Ion:

- transcript remains append-only;
- compaction/reset/handoff do not mutate old entries;
- forks naturally inherit the exact historical context controls visible at their cutoff;
- unknown entry kinds remain replayable if generic model/head/edit facets were materialized;
- there is no mutable context list that can drift from transcript history.

Costs to measure:

- cold context construction depends on candidate range, edit density, and fork depth;
- the store needs indexes for heads/ranges and fork-aware reads;
- arbitrary edit semantics would be dangerous, so Ion should expose a constrained control vocabulary.

Recommendation for Ion: adopt the principle. Store immutable transcript entries plus materialized provider-neutral projection and constrained context controls. Do not create a separately mutable canonical context vector unless benchmarks demonstrate a compelling need.

## Task authoring changed from older Pico designs

Current Pico's normative task interface is an async task definition:

```text
execute(running task, runtime, context) -> terminal closure
recover(running task, runtime, context) -> terminal closure
abort(running task, restricted runtime, context) -> abort closure
```

A task is `pending`, `running`, or `terminal`. It has immutable input, optional typed checkpoint, dependencies, ownership edges, output reference, background/turn markers, and a durable abort mark.

While an invocation is running it may perform **controlled durable commits** through its task runtime, including replacing its complete checkpoint and writing task-scoped scratch/progress. The terminal closure is later executed on the serialized line, atomically committing final writes and outcome.

The spec explicitly excludes a mandatory returned-step-plan framework. It separately allows an optional state-indexed authoring adapter for complex phased tasks; that adapter compiles to the ordinary `TaskKind` and does not add scheduler states.

### Implication for Ion P1

Ion's isolated P1 chose a stricter re-entrant `step(checkpoint) -> plan` authoring model because it made durable continuation explicit. That result remains useful evidence, but it should not be treated as final API evidence.

A stronger Rust design is likely:

```text
TaskKind
  execute(...)
  recover(...)
  abort(...)

TaskContext
  commit(... checkpoint/state ...)
  scratch(... bounded/recoverable progress ...)
  restricted ownership/wait/cancel helpers

terminal closure/plan
  atomically stores outcome + successors + retirement
```

The async invocation is **not** the durable continuation. On process loss, the future disappears and `recover` receives the durable task/checkpoint. This preserves the property P1 wanted without forcing every simple task into a scheduler-visible phase machine.

For task kinds that benefit from exhaustive phase dispatch, Ion can provide a typed state-task helper/macro that compiles to the ordinary task trait. There should still be one kernel task framework.

## Reconsidering the separate Effect object

Pico deliberately has no generic effect gate. One task is one recoverable async operation.

This is especially relevant after Ion's move from operations to tasks:

- one model generation = one task;
- one tool call = one task;
- one background job = one task;
- one collapse = one task;
- worker creation/waiting is mediated by a task and owned conversation.

A separate durable `Task -> Effect -> Attempt` hierarchy may therefore duplicate identity/lifecycle state.

Candidate Ion simplification:

```text
Task
  status: pending | running | terminal
  immutable input
  checkpoint/phase
  invocation generation
  dependencies / owned conversations
  retry/external request identity in typed checkpoint
  terminal outcome
```

Before a repeat-sensitive external action, the task durably checkpoints the exact external intent/key needed by `recover`. Retry attempt identity/usage can be retained in task-kind state or an attempt/usage ledger keyed by task + attempt. Compound operations either create child tasks or checkpoint every uncertain boundary.

Keep a separate generic `Effect` row only if a production prototype demonstrates a real case where effect identity/lifecycle must outlive or differ from its owning task. Do not preserve it solely because the old operation runtime needed one.

## pi-ai: provider/model subsystem separation

Pico's clean-room provider integration is still gated, but the existing `@earendil-works/pi-ai` package is mature evidence for component boundaries.

### Provider-neutral layer

`pi-ai` owns provider-neutral model/message/tool/stream types. The harness can store/project those types without depending on one HTTP API.

Provider-specific replay hints are retained on otherwise provider-neutral content (for example opaque thinking/text/tool-call signatures) so the producing provider can preserve continuity while another provider can ignore incompatible hints.

Ion should preserve this principle: semantic model messages should be portable; provider-specific continuation metadata should be opaque, scoped, and optional rather than leaking wire payloads into session history.

### Provider versus wire API

A `Provider` is the runtime/configuration unit:

- provider ID/name;
- auth behavior;
- model catalog/listing;
- optional dynamic catalog refresh;
- streaming behavior.

Providers can reuse lower-level **API implementations**. Examples in pi-ai include `openai-responses`, `openai-completions`, `anthropic-messages`, Google APIs, Bedrock, etc. OpenRouter, xAI, Groq, Cerebras and many local servers can share OpenAI-compatible wire implementations rather than duplicate parsing/streaming code.

This is a particularly good boundary for Ion.

Candidate shape:

```text
Model service / registry
  Providers
    auth + catalog + endpoint policy
      -> API adapter
           HTTP/SSE/WebSocket wire protocol
```

The agent kernel should not know whether a model came from OpenAI, OpenRouter, Anthropic, llama.cpp, vLLM, or a local service.

### Models collection

pi-ai's `Models` collection:

- registers providers;
- synchronously exposes the last-known provider/model catalog;
- performs explicit async refresh for dynamic providers;
- resolves provider auth;
- routes stream/complete/deferred operations to the provider owning the selected model.

Dynamic model-catalog persistence has its own small `ModelsStore`, independent from agent-session storage.

Ion should similarly keep model catalogs out of `session.sqlite`. Conversation/task state should persist the selected model reference and the exact request-relevant snapshot needed for recovery/history; the host's global catalog remains independently refreshable/replaceable.

### Auth

pi-ai makes auth provider-owned while credential persistence is app-owned through a `CredentialStore`. OAuth refresh is serialized through that store so concurrent requests do not double-refresh rotated credentials.

Ion should keep credentials outside session storage. The host/provider subsystem owns credential resolution, OAuth/API-key login, refresh and secure persistence. A session stores no secret merely because it records which provider/model was used.

### What Ion should improve rather than copy

Ion should prefer typed provider failures over string-pattern classification because durable retry/recovery policy needs stable facts. Candidate categories include transport, timeout, rate-limit/retry-after, overload/server, auth, quota/billing, invalid request, context overflow, unsupported capability, safety refusal, cancelled and unknown.

Provider adapters classify the observed failure; the generation task owns retry/backoff/compaction policy. Hidden SDK retries should be disabled or tightly controlled so durable task-attempt/usage accounting is not bypassed.

The provider interface itself should not carry `SessionId`, `TaskId`, old `OperationId`, or session mutation messages. It should accept a provider-neutral model request and return a provider-neutral stream/result. The generation task/runtime wraps that call with durable session identity and recovery semantics.

## Tool boundary

Current Pico also draws a useful trust boundary: the built-in tool task owns task-transaction authority; an ordinary `ToolDefinition` does **not** receive arbitrary task/session commit capability.

For Ion:

- the generic task/kernel owns durability, cancellation and recovery;
- ordinary file/shell/MCP tools receive a narrow execution/environment capability and return a typed result;
- the tool task records the durable attempt/checkpoint before calling them;
- special operations such as worker creation or durable job creation use narrow built-in mediated capabilities rather than handing arbitrary session mutation to every tool plugin;
- approvals bind the exact prepared invocation and happen before irreversible execution.

This supports later extensions without making an extension a second session writer.

## Recommended component boundaries for Ion

Do not freeze crate count yet, but design toward these logical boundaries:

```text
agent/session kernel
  session / conversation / entry / task / input
  storage / mutation line / recovery / observations

AI subsystem
  provider-neutral messages/model request/result
  model catalog + provider registry
  auth + credential interface
  provider adapters + reusable wire API adapters

execution environment
  workspace identity/isolation
  files / shell / processes / jobs
  tool registry + MCP adapters
  sandbox/policy

clients
  TUI / print / JSON / ACP
  consume one observation/command contract

host/application
  composes providers, environment, credentials, settings and session residency
```

The core kernel should depend only on the minimal provider-neutral types/services needed by the built-in generation behavior. HTTP clients, OAuth flows, terminal rendering and MCP transports do not belong in the session kernel.

## Current Ion implementation consequence

Current Ion provider/runtime code is structurally tied to the old operation model: provider requests/signals carry `OperationId`/step/session details, runtime retry logic is mixed into `provider.rs`, and the main runtime/store encode lanes/operations/open effects.

Because the new design removes or changes those concepts, aggressive in-place refactoring has high risk of preserving accidental coupling.

Recommended migration strategy: **clean rewrite of the core, selective port of leaf algorithms only after their new interface is accepted**.

Likely rewrite/delete with the old core:

- `agent.rs` / `agent_host.rs` durable-agent machinery;
- `operation/`;
- `runtime/`;
- `session/lane.rs`;
- most of the existing session schema/store implementation;
- current `provider.rs` runtime-facing contract;
- old context/effect orchestration.

Potential sources of algorithms/tests to port after review, not APIs to preserve:

- path/sandbox safety and output bounding in tools/process code;
- policy checks;
- provider wire parsing in OpenRouter/OpenAI-Codex adapters;
- auth flows/settings persistence;
- terminal editor/rendering primitives;
- existing fault/race tests as regression cases.

Defer/rebuild against stable boundaries:

- MCP/extensions;
- ACP/RPC;
- TUI group views;
- export/import compatibility.

## Remaining design blockers before deleting the old core

1. Finalize the async `execute/recover/abort + explicit durable commits` task contract and optional typed phase adapter in Rust.
2. Finalize immutable entry/context controls (`model projection + head + constrained edits`) and fork cutoff semantics.
3. Prototype task-level external-effect recovery without a separate `Effect` entity; retain `Effect` only if evidence requires it.
4. Choose a provisional session-local ID/commit representation for the fresh schema; keep it explicitly pre-stable.
5. Specify the minimal provider-neutral model request/stream/result contract that generation depends on, without implementing the full provider catalog yet.

After these are settled, there is little value in preserving the old runtime implementation. Git is the archive.