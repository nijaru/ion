# System Prompt

## Current direction

ion's base system prompt should be a small, stable operating-policy prompt.

It should:
- define ion as a terminal coding agent
- set tone and workflow expectations
- require repo-aware edits and verification
- stay separate from layered project instructions

It should not:
- advertise the product
- recommend specific models
- mention stale runtime/provider/backend details
- contain self-promotional language like `elite`

## Prompt surfaces

Do not mix these layers:

1. **Core system prompt**
   - ion's stable operating policy
   - identity, workflow, tool/approval policy, response style

2. **Session/runtime context**
   - dynamic facts for the current session
   - cwd, platform, date, sandbox/approval mode, git-repo presence
   - no provider/model/backend branding that can go stale in transcript UX

3. **Project instructions**
   - `AGENTS.md`, `CLAUDE.md`, and similar repo-local instruction files
   - authoritative for repo-specific conventions and workflows

4. **Skills / agent-specific prompts**
   - optional, task-matched instruction modules
   - not part of the always-on core prompt

5. **Task or mode reminders**
   - plan-mode reminders, compaction prompts, slash-command helpers, etc.
   - scoped to a mode or operation, not the base system prompt

## Comparison findings

### Claude Code

- Uses prompt fragments, not one giant product-intro prompt.
- Emphasis is on tool/permission behavior and workflow reminders.
- Strong signal: prompt content is modular and operational.
- `CLAUDE.md` is separate from the default system prompt, not the same layer.

### Codex

- Strong repo-instruction and workflow discipline.
- Focus is on concrete execution rules, verification, and codebase conventions.
- Strong signal: operating constraints matter more than product voice.

### Gemini CLI

- Prompt builder is modular and policy-heavy.
- Core sections focus on conventions, workflow, verification, sandbox, and user memory layering.
- Strong signal: base prompt should encode rules, not branding.
- Distinguishes prompt composition from memory/context loading.

### pi

- System surfaces are concise and task-shaped.
- Prompt language is practical and environment-specific, without hype.
- Strong signal: keep the system prompt compact and concrete.

### OpenCode

- Base prompt includes useful operating rules but also includes some over-branded phrasing.
- Strong signal: concise workflow rules are useful; "best coding agent on the planet" style phrasing is not.

## ion base prompt rules

- Identify ion as a terminal coding agent.
- Be concise, direct, and factual.
- Inspect relevant code, config, and tests before editing.
- Follow existing project conventions and verify dependencies before using them.
- Make small, targeted changes.
- Run relevant verification commands after edits when feasible.
- Use available tools to inspect, edit, search, and run commands.
- Verify with project-specific shell commands when feasible.
- Do not communicate through comments or command output.
- Do not revert user changes, commit, or do destructive work unless explicitly asked.
- Respect approval boundaries when tools are blocked.

## Prompt budget policy

The base prompt should stay Pi-small. The current native baseline is recorded in
`ai/research/prompt-budget-2026-05.md`: core plus runtime instructions are about
497 estimated tokens, P1 tool specs are about 961 estimated tokens, and project
instructions are the largest measured static component.

Rules:

- Do not add long formatting manuals, tool inventories, provider catalogs, or
  feature explanations to the always-on base prompt.
- Keep tool guidance short and stable; detailed tool behavior belongs in tool
  specs and docs, not in the prompt.
- Re-run `TestPromptPreludeBudgetReport` before adding default model-visible
  tools or new always-on prompt layers.
- Do not implement prompt or KV cache machinery during P1 stabilization.
  Repeated-prefix caching belongs at the provider/runtime boundary unless Ion
  owns the local inference server.

## Recommended implementation

### Architecture

Move from a single base string to a prompt builder with explicit sections:

1. `identity()`
   - one short paragraph
   - no provider/model/backend branding

2. `coreMandates()`
   - inspect before editing
   - follow repo conventions
   - do not assume dependencies or commands
   - no destructive actions unless asked

3. `workflow()`
   - inspect
   - plan
   - apply
   - verify
   - report succinctly

4. `toolPolicy()`
   - use available tools appropriately
   - respect approvals
   - do not repeat denied actions unchanged

5. `responseStyle()`
   - concise, direct, no product marketing
   - communicate in normal responses, not via comments or tool output

### Session context

Add a separate dynamic context builder for facts that are true only for the current run:
- cwd
- platform
- current date
- sandbox / approval mode
- whether the directory is a git repo

This should be distinct from the core prompt so it can evolve without rewriting core policy.

### Project instructions

Keep repo instruction files separate from the core prompt.

Current `BuildInstructions(base, cwd)` behavior is acceptable as a short-term transport mechanism, but the conceptual model should remain:
- core prompt first
- project instructions as a separate appended section

Longer term, if canto/llm supports a richer prompt structure cleanly, keep them as separate message/section sources rather than flattening everything into one undifferentiated string.

### Skills and task prompts

Do not include skill inventories or skill instructions in the always-on core prompt.
Only inject those when the runtime actually exposes and activates them.
If skills become model-visible later, prefer explicit progressive disclosure:
small host-side discovery, then `read_skill(name)` for the selected skill body.
Do not expose self-extension or marketplace install behavior from the base
prompt.

## Implementation plan

1. Refactor `internal/backend/canto/prompt.go` into section builders instead of one raw string.
2. Add a separate runtime-context builder and keep it provider/model/backend-free.
3. Keep project instruction layering explicit and documented as a separate surface.
4. Add tests that assert:
   - no stale model/provider/backend wording
   - no self-promotional language
   - prompt contains required workflow/tool/approval rules
   - runtime context is isolated from core policy
5. Add lean prompt evals for representative behaviors:
   - greeting is short and non-marketing
   - code questions cause inspection, not speculation
   - code edits imply verification
   - no stale model recommendations in general replies

## Keep out of the base prompt

- Model/provider recommendations
- Tool inventories that are likely to drift
- Backend/framework branding
- Repo-specific style rules that belong in `AGENTS.md`/`CLAUDE.md`
- Temporary product messaging

## Current implementation

- Base prompt implementation: `internal/backend/canto/prompt.go`
- Project instruction layering: `internal/backend/instructions.go`
- Review task: `tk-o6pl`
- Modularization task: `tk-qc3u` (complete)
- Prompt eval task: `tk-g060` (complete)

## Sources

- Claude Code prompt repo: `/Users/nick/github/Piebald-AI/claude-code-system-prompts`
- Codex repo: `/Users/nick/github/openai/codex`
- pi repo: `/Users/nick/github/badlogic/pi-mono`
- Gemini CLI repo: `/Users/nick/github/google-gemini/gemini-cli`
- OpenCode repo: `/Users/nick/github/anomalyco/opencode`
- Anthropic docs: [Output styles](https://code.claude.com/docs/en/output-styles), [Use XML tags](https://docs.anthropic.com/en/docs/build-with-claude/prompt-engineering/use-xml-tags), [Claude prompting best practices](https://platform.claude.com/docs/build-with-claude/prompt-engineering/claude-prompting-best-practices)
- OpenAI: [A practical guide to building agents](https://cdn.openai.com/business-guides-and-resources/a-practical-guide-to-building-agents.pdf)
