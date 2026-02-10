# Claude Code System Prompt Research

**Date:** 2026-02-09
**Version tracked:** v2.1.37 (Feb 7, 2026)
**Source:** Piebald-AI/claude-code-system-prompts (MIT, extracted from npm package)

## Overview

Claude Code's system prompt is NOT a single monolithic document. It is composed of ~133 conditional prompt segments assembled at runtime based on context (enabled tools, active mode, MCP connections, etc.). Total token budget varies by session configuration.

Anthropic does NOT officially publish the Claude Code system prompt (unlike Claude.ai chat prompts). All public versions come from extraction scripts run against the npm package.

## Architecture: Prompt Composition

| Category          | Count | Purpose                                                      |
| ----------------- | ----- | ------------------------------------------------------------ |
| Agent prompts     | 28    | Sub-agent system prompts (explore, plan, task, etc.)         |
| System prompts    | 35    | Core behavior (main, tone, tools policy, security, etc.)     |
| System reminders  | 50+   | Injected contextually (plan mode, file changes, token usage) |
| Tool descriptions | 31    | One per built-in tool                                        |
| Skills            | 3     | Debugging, config update, verification                       |
| Data templates    | 3     | GitHub actions, PR descriptions, session memory              |

Key insight: prompts use template variables like `${BASH_TOOL_NAME}`, `${READ_TOOL_NAME}`, `${TASK_TOOL_NAME}` that get interpolated at runtime.

## Main System Prompt

Identity statement:

> You are Claude Code, Anthropic's official CLI for Claude. You are an interactive CLI tool that helps users with software engineering tasks.

Key sections in the main prompt:

- Identity and purpose
- Security policy (authorized testing OK, refuse malicious)
- URL generation prohibition (never guess/fabricate URLs)
- Help resources and feedback channels
- Output style configuration (variable-driven)

## Core Behavior Prompts

### Tone and Style

- No emojis unless explicitly requested
- Short, concise responses for CLI display
- GitHub-flavored markdown, monospace rendering (CommonMark)
- Output text directly; never use bash/tools to communicate
- Never create files unless absolutely necessary; prefer editing
- No colon before tool calls ("Let me read the file." not "Let me read the file:")
- Professional objectivity: "Prioritize technical accuracy and truthfulness over validating the user's beliefs"
- No time estimates ever ("Never give time estimates or predictions for how long tasks will take")

### Doing Tasks (Software Engineering)

- NEVER propose changes to code you haven't read
- Read files first, understand conventions, mimic style
- Avoid security vulnerabilities (OWASP top 10)
- Avoid over-engineering:
  - Don't add features/refactoring beyond what was asked
  - Don't add docstrings/comments/type annotations to unchanged code
  - Don't add error handling for impossible scenarios
  - Don't create helpers for one-time operations
  - "Three similar lines of code is better than a premature abstraction"
- Delete unused code completely, no backwards-compatibility hacks

### Executing Actions with Care

- Consider reversibility and blast radius
- Local, reversible actions (editing files, running tests): proceed freely
- Risky/destructive actions: confirm with user first
  - Destructive: deleting files/branches, dropping tables, rm -rf
  - Hard to reverse: force-push, git reset --hard, amending published commits
  - Visible to others: pushing code, creating/commenting on PRs/issues, sending messages
- Don't use destructive actions as shortcuts
- Investigate unexpected state before overwriting
- "Measure twice, cut once"
- User approving an action once does NOT mean blanket authorization

### Security Policy

- Assist with authorized security testing, defensive security, CTFs, educational contexts
- Refuse destructive techniques, DoS, mass targeting, supply chain compromise, detection evasion
- Dual-use tools (C2, credential testing, exploit dev) require clear authorization context

## Tool Usage Policy

### Parallel Execution

- Call multiple tools in a single response when no dependencies exist
- Maximize parallel tool calls for efficiency
- Sequential only when outputs feed into subsequent calls
- Never use placeholders or guess missing parameters

### Tool Preference Hierarchy

- Use specialized tools over bash commands:
  - Read (not cat/head/tail)
  - Edit (not sed/awk)
  - Write (not echo/heredoc)
  - Glob (not find)
  - Grep (not grep/rg in bash)
- Reserve bash for actual system commands and terminal operations
- Never use bash echo to communicate with users

### Explore Agent for Context Gathering

- When exploring codebase for context (not needle queries), use Task tool with explore subagent
- NOT direct glob/grep for open-ended questions like "Where are errors handled?" or "What is the codebase structure?"

## Built-in Tools (18+)

### File Operations

| Tool  | Purpose                                                                                                                                         |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| Read  | Read files, images, PDFs, notebooks. Absolute paths only. cat -n format with line numbers. Up to 2000 lines default, 2000 char line truncation. |
| Write | Overwrite files. Must read first. Prefer editing existing. Never proactively create docs/READMEs.                                               |
| Edit  | Exact string replacement. Must read first. Fails if old_string not unique. replace_all for bulk. Preserve exact indentation.                    |
| Glob  | Fast pattern matching (\*_/_.js). Sorted by modification time.                                                                                  |
| Grep  | Ripgrep-based. Regex, glob/type filters. Output modes: content, files_with_matches, count. Multiline support.                                   |

### Execution

| Tool | Purpose                                                                                                                                         |
| ---- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| Bash | Shell execution with timeout. Working dir persists, shell state doesn't. Directory verification before creating files. Quote paths with spaces. |
| Task | Launch sub-agents (explore, plan, general). Background execution. Resume via agent ID.                                                          |

### Planning & Tracking

| Tool          | Purpose                                                                                                     |
| ------------- | ----------------------------------------------------------------------------------------------------------- |
| EnterPlanMode | Switch to planning mode (read-only)                                                                         |
| ExitPlanMode  | Exit planning mode                                                                                          |
| TodoWrite     | Task tracking for 3+ step work. States: pending, in_progress, completed. Exactly ONE in_progress at a time. |
| Skill         | Execute skills (slash commands like /commit, /review-pr). Must invoke before generating response.           |

### Web & Search

| Tool      | Purpose                                                                                                  |
| --------- | -------------------------------------------------------------------------------------------------------- |
| WebSearch | Real-time web search. Must include Sources section with URLs. Domain filtering. Correct year in queries. |
| WebFetch  | Fetch URL content, process with AI. 15-min cache. Handles redirects.                                     |

### Communication

| Tool            | Purpose                           |
| --------------- | --------------------------------- |
| AskUserQuestion | Ask user for clarification        |
| SendMessageTool | Send messages (team coordination) |

### Other

| Tool         | Purpose                              |
| ------------ | ------------------------------------ |
| Computer     | Browser automation (Chrome)          |
| LSP          | Language Server Protocol integration |
| NotebookEdit | Jupyter notebook cell editing        |
| Sleep        | Pause execution                      |
| ToolSearch   | Find additional tools/plugins        |
| TeammateTool | Coordinate with team agents          |

## Sub-Agent Architecture

### Explore Agent

- Read-only mode, no file modifications
- Tools: glob, grep, read, bash (read-only only)
- Purpose: search and analyze existing code
- Returns findings with absolute file paths

### Plan Mode Agent

- "Software architect and planning specialist"
- Strictly read-only: no creating, editing, deleting files
- 4-stage process: understand requirements, explore code, design solutions, detail implementation
- Must conclude with 3-5 critical files relevant to implementation
- Bash restricted to read-only (ls, git status, cat)

### Task Agent (General)

- Full tool access for code search and codebase analysis
- Can write files when instructed
- Completes tasks and delivers detailed writeup
- Trust agent outputs generally

## Git & PR Workflow

### Git Safety Protocol

- NEVER update git config
- NEVER run destructive commands (push --force, reset --hard, checkout ., clean -f) without explicit request
- NEVER skip hooks (--no-verify) without explicit request
- NEVER force push to main/master (warn if requested)
- Always create NEW commits rather than amending (amend modifies PREVIOUS commit after hook failure)
- Prefer staging specific files over git add -A/.
- Never commit unless explicitly asked

### Commit Process

1. Parallel: git status, git diff, git log (recent messages for style)
2. Analyze all staged changes, draft concise 1-2 sentence message (why not what)
3. Add files, commit with HEREDOC message format, verify with git status

### PR Creation

1. Parallel: git status, git diff, git log, git diff base...HEAD
2. Analyze ALL commits (not just latest), draft title (<70 chars) and summary
3. Push with -u, create via gh pr create with HEREDOC body (Summary + Test plan)

## Task Management (TodoWrite)

### When to Use

- Tasks with 3+ distinct steps
- Non-trivial work requiring planning
- User requests todo lists
- Multiple tasks provided
- Any work session start

### Rules

- Exactly ONE task in_progress at any time
- Mark complete only when fully accomplished (tests pass, no errors)
- Use frequently for visibility
- Mark done immediately, don't batch

## Skill System

- Skills = slash commands (/commit, /review-pr, /pdf, etc.)
- BLOCKING requirement: invoke Skill tool BEFORE generating any response
- Never mention a skill without calling the tool
- Available skills listed in system-reminder messages
- Skills loaded via `<skill>` tags in conversation turn

## Agent Memory

- Agents can build knowledge across conversations
- Domain-specific memory instructions (code patterns, test patterns, architecture)
- Triggered by "memory", "remember", "learn", "persist" mentions
- Writes concise notes about discoveries

## Session Management

- Compact: summarize conversation while preserving context
- Clear: remove conversation history entirely
- Session continuation reminders injected when resuming
- Token usage reminders at thresholds

## Notable Design Patterns

1. **Template variables throughout** - prompts are parameterized, not hardcoded
2. **Conditional injection** - system reminders appear only when relevant (plan mode, file changes, etc.)
3. **Sub-agent isolation** - explore/plan agents are explicitly read-only
4. **Trust hierarchy** - user instructions in CLAUDE.md can override defaults, but safety guardrails remain
5. **Progressive disclosure** - not all tools shown upfront; ToolSearch for discovering more
6. **Scratchpad directory** - ephemeral workspace for agent computation
7. **Learning mode** - optional collaborative mode requesting human input for significant decisions

## Key Sources

- [Piebald-AI/claude-code-system-prompts](https://github.com/Piebald-AI/claude-code-system-prompts) - Full extraction, 133 files, MIT license
- [chigkim gist](https://gist.github.com/chigkim/1f37bb2be98d97c952fd79cbb3efb1c6) - Single-document compilation
- [transitive-bullshit gist](https://gist.github.com/transitive-bullshit/487c9cb52c75a9701d312334ed53b20c) - Unminified prompts + tool definitions
- [EliFuzz/awesome-system-prompts](https://github.com/EliFuzz/awesome-system-prompts) - Collection across multiple agents
- [Simon Willison's analysis](https://simonwillison.net/2025/May/25/claude-4-system-prompt/) - Commentary on Claude 4 system prompt
- [anthropics/claude-code](https://github.com/anthropics/claude-code) - Official repo (prompts not in source)
- [Issue #4141](https://github.com/anthropics/claude-code/issues/4141) - Clarification that prompt is not public
