# Dynamic Configuration & Model Metadata

Archived on 2026-03-28.

This note mixed:

- old status/footer design discussion
- dynamic metadata rollout notes
- config migration guidance

It has been superseded by smaller current docs:

- `ai/specs/status-and-config.md`
- `ai/specs/model-catalog-strategy.md`
- `ai/specs/tui-architecture.md`

Original content preserved below.

---

# Dynamic Configuration & Model Metadata

**Date:** 2026-03-26
**Tasks:** tk-810x (status line polish), tk-rd3q (token usage / cost display), tk-7gey (model config), tk-usvn (/model and /provider commands)
**Context:** Ion's transition from hardcoded model defaults and context limits to a dynamic, user-configurable system with high-fidelity TUI parity.

---

## TUI High-Fidelity Specs (INCOMPLETE)

**Goal:** Match the Rust-era `status_bar.rs` logic and visual density closely, while improving UX where it helps.

### 1. Progress Line (Top)

- **Active:** `⠋ Thinking...` (Braille spinner `spinner.Dot`).
- **Idle:** `· Ready` or a restored status message from the resumed session.
- **Resume UX:** On `--continue` / `--resume`, restore the prior progress line content if the session has one. Prepend the transcript with `--- resumed ---` and then a startup metadata line so the boundary is obvious.
- **Constraint:** NO emojis. Clean text symbols only.

### 2. Composer Separator

- Plain `─────` separator. No mode indicator punched in — looked cluttered in practice.
- Bars are dim/faint. Clean boundary between Plane B and composer.

### 3. Status Line (Bottom)

- **Format:** `{mode} · {provider} · {model} · {usage} · {cost} · {dir} · {branch}`
- **Mode Indicator:** `[WRITE]` / `[READ]` at the start of the status line. `[WRITE]` is yellow, `[READ]` is cyan.
- **Model Display:** Clean handling of provider/model combinations such as `openrouter/minimax/m2.7`.
- **Model Display:** Clean handling of provider/model combinations such as `openrouter/minimax/m2.7`.
- **Usage Format:** `45% (4k/128k)` if limit is known, else `4k`.
- **Cost Format:** Append `($0.02)` if > 0.
- **Dynamic Truncation:** Drop segments in priority order (Dir → Branch → Provider) to prevent wrapping while preserving the most useful information.
- **Narrow Width UX:** Prefer truncation over wrapping; keep the line readable before trying to show everything.

---

## Infrastructure Decisions

### 1. Model Metadata

- **CRITICAL: No hardcoded metadata.**
- Metadata must be retrieved from `charmbracelet/catwalk` or dynamic provider APIs (e.g. OpenRouter `/models`) and cached locally.
- The registry should surface the data needed for status-line usage, pricing, and model display without embedding tables in code.

### 2. Configuration

- `~/.ion/config.toml` is the user-facing source of truth for provider, model, optional context override, and session retention.
- Provider and model may be empty in the file while the user is editing, but ion must not auto-fill them on startup or invent defaults.
- Startup validation happens at runtime-open time. Native and ACP both require a model before ion can start a session.
- TOML is preferred here because the file is small, human-edited, comment-friendly, and close to the Rust-era workflow without needing JSON migrations.
- Commands that change the model or provider MUST update `config.toml` because those are user actions.
- `ION_PROVIDER` and `ION_MODEL` remain startup overrides only and must not be persisted automatically.
- Hardcode ion-owned defaults like data dir and metadata cache TTL unless a future machine-owned value truly needs its own persisted state.
