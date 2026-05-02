# ion Decisions

Distilled architectural principles plus recent decision log.

## Principles

- Native Ion is the product baseline; ACP and subscription bridges are secondary
  compatibility paths.
- Canto owns framework mechanisms: durable events, provider-visible history,
  agent/tool lifecycle, retry/cancel settlement, compaction primitives, and
  provider transforms.
- Ion owns product policy: TUI/CLI shell, commands, settings/state, provider
  choice, display projection, workspace policy, and coding-tool UX.
- Keep the core tool surface small and deliberate. The P1 set is `bash`,
  `read`, `write`, `edit`, `multi_edit`, `list`, `grep`, and `glob`.
- Research is evidence; specs and root files are canonical.
- Do not add a global alternate code path for stabilization. Deferred features
  must be absent or rejected at their owning boundary.
- Scriptable CLI behavior is a first-class regression surface.
- Commit each coherent green slice; do not push without explicit approval.

## Recent Log

### 2026-05-02 - Edit surface stays split after I2 evaluation

Pi's merged `edit(path, edits[])` is the best future simplification candidate,
but Ion should keep `write`, `edit`, and `multi_edit` for the current I4
surface. The split tools are already hardened around exact replacement, CRLF/BOM
preservation, line-numbered errors, expected replacement counts, and atomic
validation. A merged edit surface needs eval evidence across single-file,
multi-file, overlap, duplicate, CRLF/BOM, cancellation, and
provider-compatibility cases before replacing working tools.

### 2026-05-02 - AI context becomes design-first

The next Ion work stream starts by pruning `ai/` and rewriting root files around
the full product design, not just the old core-loop flag. Root `ai/` stays to
five canonical files. Topic docs are evidence only.

### 2026-05-02 - Native baseline is single-path

The old global stabilization split is removed. The current native P1 behavior
is the normal path: eight default tools, compact tool display,
provider-history request capture, and deferred ACP/MCP/memory/subagent/trust/
policy surfaces rejected at their owning boundaries.

### 2026-05-01 - Tool research is distilled into specs

Tool-surface and prompt-budget research stays as evidence. Durable behavior
lives in `ai/specs/tools-and-modes.md` and `ai/specs/system-prompt.md`.

### 2026-05-01 - Dedicated search tools stay

Ion keeps `grep`, `glob`, and `list` as dedicated read-only tools for display,
truncation, path policy, and approval boundaries. `rg` semantics remain the
near-term baseline; ripgo is a later benchmarked integration.

### 2026-05-01 - Structured edits stay

Ion keeps `edit`, `multi_edit`, and `write` for the current tool surface.
Expected replacement counts, line-numbered ambiguity errors, CRLF/BOM-safe
matching, and atomic validation are the reliability priorities. A merged
`edit(edits[])` surface is deferred.

### 2026-05-01 - Model-visible truncation must be explicit

Native tool results pass through one shared output limiter. Truncated model
observations include an explicit marker with byte counts and recovery guidance.

### 2026-05-01 - Read output carries line numbers

The model-visible `read` result includes cat-style line numbers for edit
precision. The TUI still summarizes routine read rows by default.

### 2026-05-01 - Verification uses bash by default

The dedicated `verify` tool is removed from the default native registry.
Ordinary tests/builds/lints run through `bash`; structured verification is
deferred until an eval/RLM feature needs it.

### 2026-05-01 - Roadmap sequencing

Ion proceeds as: stable native agent, minimal TUI/CLI shell, safety/product
table stakes, then advanced framework and SOTA surfaces. Canto reopens only for
concrete framework-owned failures found by Ion.

### 2026-04-30 - Busy input queues by default

Queued follow-up remains the default. True active-turn steering waits for a
Canto boundary-step contract so Ion never inserts user text into invalid
provider-visible history.

### 2026-04-27 - Core parity gates feature work

Pi/Codex/Claude parity is a staged reliability baseline, not a feature
checklist. Native submit/stream/tool/cancel/error/persist/replay correctness
blocks provider polish, ACP, subscriptions, skills, branching, and routing.

### 2026-04-27 - Runtime retry until cancellation

Transient provider/network failures retry until user cancellation by default.
Canto owns retry mechanics; Ion owns the setting, visible retry status, and
persisted status rows.

### 2026-04-27 - Thinking controls are capability-driven

Ion exposes a small reasoning vocabulary. Canto/provider adapters translate it
to provider-native request fields or omit unsupported parameters.
