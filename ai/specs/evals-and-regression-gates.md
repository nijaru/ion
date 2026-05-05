# Evals and Regression Gates

## Position

Ion evals are versioned product checks over the host behavior, prompt surface, and
coding workflows. Canto owns the reusable `x/eval` primitives; Ion owns the golden
datasets and thresholds that define whether this product regressed.

DSPy/GEPA-style optimization belongs downstream of these artifacts. Runtime prompts
must not mutate themselves during normal Ion use.

## Minimal Harness Contract

The native acceptance floor is the Canto-backed path:

```text
Ion TUI/CLI -> CantoBackend -> Canto session/runtime/agent/tools -> provider API
```

Every refactor must preserve these invariants:

- provider-visible history is valid: no empty assistant payloads, no orphan tool
  results, and no display-only Ion rows in model requests
- Canto owns model-visible transcript persistence; Ion owns display projection
- event ordering is stable: turn start, assistant/tool events, terminal state,
  then ready/follow-up
- cancel, provider error, tool error, budget/limit stop, and compaction failure
  leave a resumable session
- replay equals live for display rows while provider-visible history remains
  exact
- approval pauses and resumes a specific pending request without losing
  in-flight state
- print mode exercises the same native loop as the TUI
- runtime switches are atomic enough that failed switches do not corrupt the
  previous session or provider state

## Initial Gate

| Dataset | Enforced by | Purpose |
|---|---|---|
| `evals/golden/prompt_quality.toml` | `go test ./internal/backend/canto` | Keeps the coding-agent system prompt concise, grounded, verification-oriented, and free of stale provider/model recommendations. |

This is intentionally small. It proves the shape:

1. golden cases are plain files versioned with code
2. ordinary tests enforce them in CI
3. failures name the missing or forbidden prompt text

## Required Artifact Shape

Future golden datasets should be machine-readable and optimizer-readable:

| Field | Purpose |
|---|---|
| `id` / `name` | stable case identity |
| `instruction` | user-facing task |
| `environment` | local fixture, harness, or external connector |
| `expected_tools` | action-layer checks |
| `forbidden_tools` | safety/regression checks |
| `scores` | scalar thresholds |
| `subscores` | named partial-credit dimensions |
| `feedback` | textual judge feedback for future optimizers |

The durable session log remains the source of trajectory truth. Golden files should
describe expectations, not duplicate transcript data.

## Near-Term Suites

| Suite | Priority | Gate |
|---|---:|---|
| prompt quality | P0 | unit test |
| minimal harness acceptance | P0 | deterministic app/CLI tests plus optional tmux/live smoke |
| permission policy | P0 | unit/integration tests over `PolicyEngine` and TUI approvals |
| tool lifecycle | P0 | fake backend integration tests |
| bug workflow | P1 | local fixture with reproduce-before-fix check |
| review workflow | P1 | local fixture with severity-ranked findings |
| provider behavior | P2 | smoke tests only; no broad matrix by default |

## CI Policy

- Every PR runs deterministic local gates through `go test ./...`.
- The minimal harness gate is the product acceptance floor: submit, stream,
  tool call/result, compact display, final transcript rendering, durable
  replay, scriptable print mode, and optional tmux/live smoke.
- Tmux smoke should include a wide-to-narrow resize check. Inspect visible pane
  state for one idle `Ready` row, two shell separators, and no stale progress
  fragments; full scrollback may retain historical rows because Ion runs in
  inline mode.
- LLM judge or external harness gates are opt-in until cost controls exist.
- Any expensive gate must write JSONL results compatible with Canto `x/eval`.
- A failed eval blocks prompt/tool/workflow changes the same way a failed unit test
  blocks code changes.

## Deferred

- autoresearch loops
- prompt mutation or optimizer-driven runtime prompt edits
- broad provider/model matrix gates
- paid external harnesses in default CI
- mandatory LLM judges on every PR
