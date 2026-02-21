# Claude Code Skills System: Deep Dive (February 2026)

**Date**: 2026-02-19
**Purpose**: Exact paths, formats, and invocation mechanics for Claude Code skills
**Sources**: Official Anthropic docs (code.claude.com), reverse-engineering blog posts, Anthropic skills repo

---

## 1. Exact Discovery Paths

| Priority    | Location   | Path Pattern                                                         | Scope                |
| ----------- | ---------- | -------------------------------------------------------------------- | -------------------- |
| 1 (highest) | Enterprise | Managed settings (macOS: `/Library/Application Support/ClaudeCode/`) | All org users        |
| 2           | Personal   | `~/.claude/skills/<skill-name>/SKILL.md`                             | All user projects    |
| 3           | Project    | `.claude/skills/<skill-name>/SKILL.md`                               | This project only    |
| 4 (lowest)  | Plugin     | `<plugin>/skills/<skill-name>/SKILL.md`                              | Where plugin enabled |

**Conflict resolution**: Higher priority wins when names match. Plugin skills use `plugin-name:skill-name` namespace (no conflicts with other levels).

**Legacy commands**: `.claude/commands/<name>.md` still works. If a skill and command share the same name, the skill takes precedence.

**Nested discovery**: When editing `packages/frontend/src/foo.ts`, Claude Code also scans `packages/frontend/.claude/skills/`. Supports monorepo setups.

**Additional directories**: Skills in `.claude/skills/` within `--add-dir` directories are loaded automatically with live change detection (editable mid-session).

---

## 2. SKILL.md Format

### Frontmatter Fields (Complete)

```yaml
---
name:
  my-skill # Optional. Lowercase letters, numbers, hyphens. Max 64 chars.
  # If omitted, uses directory name. Becomes the /slash-command.
description:
  What it does # Recommended. Claude uses this to decide when to auto-invoke.
  # If omitted, uses first paragraph of markdown content.
argument-hint: "[issue-number]" # Optional. Shown during autocomplete.
disable-model-invocation: true # Optional. Default: false. If true, only user can invoke via /name.
user-invocable:
  false # Optional. Default: true. If false, hidden from / menu.
  # Only Claude can invoke it.
allowed-tools:
  Read, Grep, Glob # Optional. Tools Claude can use without permission when skill active.
  # Supports wildcard: Bash(git *)
model: sonnet # Optional. Override model for this skill.
context: fork # Optional. Set to "fork" to run in isolated subagent.
agent:
  Explore # Optional. Which subagent type when context: fork.
  # Options: Explore, Plan, general-purpose, or custom from .claude/agents/
hooks: # Optional. Lifecycle hooks scoped to this skill.
  PreToolUse:
    - matcher: "Bash"
      hooks:
        - type: command
          command: "./scripts/validate.sh"
---
Markdown instructions here...
```

### Invocation Matrix

| Frontmatter                      | User can invoke | Claude can invoke | Context loading                                              |
| -------------------------------- | --------------- | ----------------- | ------------------------------------------------------------ |
| (default)                        | Yes (via /name) | Yes (auto)        | Description always in context, full SKILL.md loads on invoke |
| `disable-model-invocation: true` | Yes             | No                | Description NOT in context                                   |
| `user-invocable: false`          | No              | Yes               | Description always in context                                |

### String Substitutions

| Variable               | Description                                      |
| ---------------------- | ------------------------------------------------ |
| `$ARGUMENTS`           | All arguments passed when invoking               |
| `$ARGUMENTS[N]`        | Specific argument by 0-based index               |
| `$N`                   | Shorthand for `$ARGUMENTS[N]` (e.g., `$0`, `$1`) |
| `${CLAUDE_SESSION_ID}` | Current session ID                               |

### Dynamic Context Injection

The `` !`command` `` syntax runs shell commands before skill content is sent to Claude:

```yaml
---
name: pr-summary
context: fork
agent: Explore
---
PR diff: !`gh pr diff`
Changed files: !`gh pr diff --name-only`
```

Commands execute immediately (preprocessing). Claude only sees the output.

### Extended Thinking

Include the word "ultrathink" anywhere in skill content to enable extended thinking.

---

## 3. Skill Directory Structure

```
my-skill/
  SKILL.md           # Required - main instructions
  template.md        # Optional - template for Claude to fill in
  reference.md       # Optional - detailed API docs (loaded when needed)
  examples/
    sample.md        # Optional - example output
  scripts/
    validate.sh      # Optional - script Claude can execute
    helper.py        # Optional - utility script
```

Reference supporting files from SKILL.md so Claude knows what they contain:

```markdown
## Additional resources

- For complete API details, see [reference.md](reference.md)
- For usage examples, see [examples.md](examples.md)
```

Best practice: Keep SKILL.md under 500 lines. Move detailed reference to separate files.

---

## 4. Internal Invocation Mechanism

### The Skill Tool

Claude Code exposes a meta-tool called `Skill` in the tools array (not the system prompt).

**Tool definition**:

```javascript
{
  name: "Skill",
  inputSchema: {
    command: string  // skill name, e.g., "pdf", "explain-code"
  }
}
```

### Available Skills List

At session start, Claude Code builds an `<available_skills>` block injected into the Skill tool's description:

```
<available_skills>
"explain-code": Explains code with visual diagrams and analogies. Use when explaining how code works.
"deploy": Deploy the application to production
</available_skills>
```

Format: `"name": description` (with optional `- when_to_use` appended if present).

Skills are filtered from this list if:

- `disable-model-invocation: true` is set
- They exceed the character budget

**Character budget**: 2% of context window, fallback 16,000 characters. Override with `SLASH_COMMAND_TOOL_CHAR_BUDGET` env var.

### Prompt Injection Mechanism

When a skill is invoked, two messages are injected into conversation history:

**Message 1 (Visible, isMeta: false)**:

```xml
<command-message>The "explain-code" skill is loading</command-message>
<command-name>explain-code</command-name>
<command-args>src/auth/login.ts</command-args>
```

**Message 2 (Hidden, isMeta: true)**:
Full SKILL.md body (frontmatter stripped), with $ARGUMENTS substituted.

### Context Modifier

The skill's `allowed-tools` are injected as pre-approved tools via a context modifier:

```javascript
contextModifier(context) {
  // Inject allowed tools into alwaysAllowRules
  // Override model if specified
  return modified;
}
```

Pre-approval is scoped to the skill's execution duration only.

### Decision Mechanism

Pure LLM reasoning. No algorithmic skill selection, no embeddings, no classifiers. Claude reads the `<available_skills>` list as plain text and uses transformer reasoning to match user intent to skill descriptions.

---

## 5. Relationship: Skills vs Commands vs Agents

### Complete .claude/ Directory Structure

```
~/.claude/                        # User-level
  CLAUDE.md                       # User instructions (always loaded)
  settings.json                   # User preferences
  skills/                         # User skills
    skill-name/
      SKILL.md
  commands/                       # Legacy slash commands (still work)
    command-name.md
  agents/                         # Subagent definitions
    agent-name.md
  agent-memory/                   # Persistent subagent memory
    agent-name/
      MEMORY.md

.claude/                          # Project-level
  settings.json                   # Project config
  settings.local.json             # Local overrides (gitignored)
  .mcp.json                       # MCP servers
  skills/                         # Project skills
    skill-name/
      SKILL.md
  commands/                       # Legacy project commands
  agents/                         # Project subagent definitions
  agent-memory/                   # Project subagent memory (committed)
  agent-memory-local/             # Local subagent memory (gitignored)
  rules/                          # Path-specific rules

CLAUDE.md                         # Project instructions (root)
```

### Comparison

| Aspect           | Skills                                    | Legacy Commands              | Subagents                                             |
| ---------------- | ----------------------------------------- | ---------------------------- | ----------------------------------------------------- |
| Path             | `.claude/skills/<name>/SKILL.md`          | `.claude/commands/<name>.md` | `.claude/agents/<name>.md`                            |
| Invocation       | Auto (description match) + manual `/name` | Manual `/name` only          | Auto delegation by Claude                             |
| Context          | Main conversation (inline) or forked      | Main conversation            | Separate context window                               |
| Supporting files | Yes (directory)                           | No (single file)             | No (single file)                                      |
| Frontmatter      | Full set (see above)                      | Same frontmatter supported   | Different fields (tools, model, permissionMode, etc.) |
| Auto-discovery   | Yes (via description)                     | No                           | Yes (via description)                                 |

### How Skills + Agents Interact

| Approach                     | System prompt                         | Task                        | Also loads                   |
| ---------------------------- | ------------------------------------- | --------------------------- | ---------------------------- |
| Skill with `context: fork`   | From agent type (Explore, Plan, etc.) | SKILL.md content            | CLAUDE.md                    |
| Subagent with `skills` field | Subagent's markdown body              | Claude's delegation message | Preloaded skills + CLAUDE.md |

Subagent `skills` field injects full skill content at startup (not just available for invocation). Subagents do NOT inherit skills from parent conversation.

### Built-in Subagents

| Agent           | Model   | Tools     | Purpose                              |
| --------------- | ------- | --------- | ------------------------------------ |
| Explore         | Haiku   | Read-only | File discovery, code search          |
| Plan            | Inherit | Read-only | Codebase research for planning       |
| general-purpose | Inherit | All       | Complex multi-step tasks             |
| Bash            | Inherit | Terminal  | Running commands in separate context |

---

## 6. Permission Control

### Skill Tool Permissions

In `/permissions` or settings.json:

```
# Deny all skills
Skill

# Allow specific skills
Skill(commit)
Skill(review-pr *)     # prefix match

# Deny specific skills
Skill(deploy *)
```

### Environment Variables

| Variable                                       | Purpose                                                     |
| ---------------------------------------------- | ----------------------------------------------------------- |
| `SLASH_COMMAND_TOOL_CHAR_BUDGET`               | Override character budget for skill descriptions in context |
| `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS`         | Set to `1` to disable background subagents                  |
| `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`              | Trigger compaction earlier (e.g., `50` for 50%)             |
| `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD` | Set to `1` to load CLAUDE.md from --add-dir directories     |

---

## 7. Compatibility Notes for ion

### What ion Already Has

From `/Users/nick/github/nijaru/ion/src/skill/mod.rs`:

- YAML frontmatter parsing (name, description, allowed-tools, model/models)
- Progressive disclosure (summary at startup, full on demand)
- SkillRegistry with lazy loading
- Legacy XML format support
- Directory scanning (`skill-name/SKILL.md` pattern)

### Gaps vs Claude Code

| Feature                           | Claude Code             | ion                         |
| --------------------------------- | ----------------------- | --------------------------- |
| `disable-model-invocation`        | Yes                     | No                          |
| `user-invocable`                  | Yes                     | No                          |
| `argument-hint`                   | Yes                     | No                          |
| `context: fork`                   | Yes                     | No (no subagent system yet) |
| `agent` field                     | Yes                     | No                          |
| `hooks` field                     | Yes                     | No                          |
| `$ARGUMENTS` substitution         | Yes                     | No                          |
| `!`command`` preprocessing        | Yes                     | No                          |
| Character budget for descriptions | Yes                     | No                          |
| Nested directory discovery        | Yes                     | No                          |
| `/skill-name` invocation          | Yes                     | Unknown                     |
| Commands compatibility            | Yes (.claude/commands/) | N/A                         |

### agentskills.io Spec vs Claude Code Extensions

The agentskills.io spec defines FORMAT ONLY:

- name (required, 1-64 chars, lowercase + hyphens)
- description (required, 1-1024 chars)
- license (optional)
- compatibility (optional)
- metadata (optional)
- allowed-tools (optional, experimental, space-delimited)

Claude Code extensions beyond the spec:

- disable-model-invocation
- user-invocable
- argument-hint
- context
- agent
- hooks
- model
- All the invocation/discovery mechanics

---

## References

- Official docs: https://code.claude.com/docs/en/skills
- Subagents docs: https://code.claude.com/docs/en/sub-agents
- Anthropic skills repo: https://github.com/anthropics/skills
- Deep dive blog: https://leehanchung.github.io/blogs/2025/10/26/claude-skills-deep-dive/
- Internals blog: https://mikhail.io/2025/10/claude-code-skills/
- Customization guide: https://alexop.dev/posts/claude-code-customization-guide-claudemd-skills-subagents/
