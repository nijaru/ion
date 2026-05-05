# Testing Model Ladder

Updated: 2026-03-26

This note defines which parts of ion can be tested without a live model, which parts benefit from a cheap live model, and which models we should prefer when we do need live coverage.

The short version:

- Most of the TUI can be tested automatically.
- Core update/render/state logic should be unit or integration tested with fake backends.
- Live-model tests should be reserved for smoke coverage and provider/tool-call verification.

---

## What We Can Test Automatically

### 1. Pure TUI logic

These should not need a live model:

- status line rendering
- markdown formatting and truncation
- picker layout and navigation
- slash command parsing
- resize behavior
- startup banner generation

Preferred test style:

- table-driven unit tests
- snapshot-style assertions for rendered strings
- narrow width regression cases

### 2. Host model logic

These can be tested with fake sessions/backends:

- `internal/app.Model.Update`
- turn submission and cancel flows
- startup/resume state transitions
- picker state transitions
- direct `/provider` and `/model` command behavior
- tool-specific UI flows as each tool is added

Preferred test style:

- integration tests against fake session interfaces
- event-driven tests that feed `tea.Msg` sequences
- no live network calls

### 3. Backend plumbing

These should also be automated:

- startup validation
- config load/save
- provider routing
- ACP event mapping
- session persistence behavior

Preferred test style:

- unit tests for routing and validation
- in-process fake backend tests
- temp-dir backed storage tests

---

## What Should Use Live Models

Live models are for smoke tests, not for the bulk of coverage.

Use them when we need to prove:

- a real provider can start a session
- tool calls round-trip correctly
- resume/startup behavior works end to end
- markdown output stays readable with real model output
- representative provider/model switching preserves usable session context
- ACP and native paths both stay usable against real providers

Most providers and models should work through the shared API paths once the core provider adapters are solid. We do not need to test every single combination; instead, cover the common API families and any provider that materially differs in request shape, streaming behavior, caching, or tool support.

These tests should be short, deterministic where possible, and cheap to run.

---

## Recommended Test Ladder

### Tier 0: No model

Use this for most CI and local development.

- fake backend
- fake session store
- synthetic events
- render-only checks

### Tier 1: Cheap reliable live model

Default choice:

- `deepseek/deepseek-v3.2`

Why:

- cheap enough for repeated smoke runs
- good enough to catch integration regressions
- suitable as the default live model for automated verification

### Tier 2: Stronger live model

Use as the manual alternate if a smoke failure looks model-quality related:

- `openai/gpt-5.4-nano`

Why:

- similar cost to `minimax/minimax-m2.7`
- slightly stronger on coding benchmarks
- good manual retry model when the cheap tier gives an ambiguous failure

### Tier 3: Free experiment model

Try only if tool calling and session behavior are confirmed:

- `stepfun/step-3.5-flash:free`

Why:

- free is attractive for repeated smoke loops
- only worth using if it supports the features ion needs without extra friction

### Reference point

- `minimax/minimax-m2.7` is a useful cost comparison point
- it is not the default test model unless we explicitly decide it is the best tradeoff

---

## Suggested Default Policy

For automated smoke coverage:

1. Probe Fedora local-api first when it is reachable and not in active use.
2. If Fedora is unavailable or times out, run the OpenRouter fallback instead
   of deferring the gate.
3. Use `deepseek/deepseek-v3.2` as the default OpenRouter fallback.
4. If the fallback fails in a way that looks like model quality, rerun manually with `openai/gpt-5.4-nano`.
5. If we want to experiment with an even cheaper path, try `stepfun/step-3.5-flash:free` manually.
6. Do not expand live coverage into every provider/model pair; cover representative transitions instead.

For CI:

- prefer Tier 0 and fake-backend coverage
- keep live-model smoke tests optional or narrowly targeted

For local validation:

- run a quick Tier 0 pass first
- then run one cheap live smoke path if we changed startup, selection, or ACP/native wiring

### Live smoke harness

- Location: `cmd/ion/live_smoke_test.go`
- Gate: `ION_LIVE_SMOKE=1`
- Default provider: `openrouter`
- Default model: `deepseek/deepseek-v3.2`
- Optional overrides:
  - `ION_SMOKE_PROVIDER`
  - `ION_SMOKE_MODEL`
  - `ION_SMOKE_PROMPT`
- Manual retry model when the cheap tier is ambiguous:
  - `openai/gpt-5.4-nano`
- Local primary when Fedora is reachable/free:
  - provider `local-api`
  - model `qwen3.6:27b`
- This harness proves the native/backend loop only; the TUI is covered separately in `internal/app` tests.
- Provider/model swap behavior should be verified by targeted tests and one representative smoke path, not a full matrix.
- The harness auto-approves tool requests so the prompt can exercise the real tool path without manual input.
- It should stay short, deterministic where possible, and skip cleanly when credentials are not present.

---

## Practical Coverage Targets

If we want confidence that the core agent works, the minimum live smoke should prove:

- ion boots
- provider/model are reported correctly
- one turn can be submitted
- one agent response streams through
- one tool call works
- turn completion is persisted
- resume shows the session boundary clearly

That is enough to prove the agent loop is usable without needing broad live coverage for every UI detail.
