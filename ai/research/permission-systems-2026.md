# Permission Systems in Coding Agents (February 2026)

## Agent Comparison

| Agent       | Default            | Modes                                                     | CLI Flags                                     | Config Rules                                     | OS Sandbox                     |
| ----------- | ------------------ | --------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------ | ------------------------------ |
| Claude Code | Ask per tool       | 6 (default, acceptEdits, plan, delegate, dontAsk, bypass) | `--dangerously-skip-permissions`              | Glob patterns: `Bash(npm *)`, deny > ask > allow | Seatbelt + bubblewrap          |
| Codex CLI   | Workspace sandbox  | 3 sandbox + 4 approval                                    | `--sandbox`, `--ask-for-approval`             | TOML config                                      | Landlock + seccomp + Seatbelt  |
| Gemini CLI  | Ask per tool       | Default + YOLO                                            | `--yolo`, `--sandbox`                         | JSON: `autoAccept`, `safeTools`                  | Seatbelt (5 profiles) + Docker |
| Amp         | Rule-based         | allow/reject/ask/delegate                                 | `--dangerously-allow-all`                     | JSON rules, first-match-wins, regex              | None                           |
| opencode    | Config-based       | allow/ask/deny per tool                                   | None (config only)                            | JSON: glob patterns, last-match-wins             | None                           |
| Droid       | Read-only          | 4 tiers (none/low/med/high)                               | `--auto <level>`, `--skip-permissions-unsafe` | None                                             | None                           |
| Pi-mono     | Everything allowed | None (YOLO default)                                       | `--tools <list>` (restrict tool set)          | None                                             | None                           |

## User Behavior Data

Source: UpGuard analysis of 18,470 public Claude Code configs + Reddit/HN/GitHub issues.

| User Segment        | %       | Behavior                                                          |
| ------------------- | ------- | ----------------------------------------------------------------- |
| Full YOLO           | ~20-25% | `--dangerously-skip-permissions` always, aliased to short command |
| YOLO + container    | ~15-20% | Auto-approve inside Docker/devcontainer, git as undo              |
| Reluctant approvers | ~30-40% | Default mode, gradually add allow rules, approval fatigue         |
| Cautious/enterprise | ~10-15% | Genuinely review each action, managed settings                    |

Key findings:

- Only 1.1% of configs had any deny rules (one-way ratchet toward less security)
- Sandboxing reduced permission prompts by 84% (Anthropic internal data)
- 30-50 prompts/hour in default mode, <15 min flow state
- Users think in intent ("allow git") but systems prompt for exact strings

## Industry Direction

Converging on three layers:

1. **OS sandbox** — hard boundary, prevents catastrophic damage
2. **Config allowlists** — pattern matching for common operations
3. **Rare, high-signal prompts** — only for genuinely novel/dangerous actions

Permission prompts as primary security = failed model. Sandbox-as-default is the trend.

## OS Sandbox Options for Rust

| Platform           | Mechanism                          | Integration                                 | Used By                    |
| ------------------ | ---------------------------------- | ------------------------------------------- | -------------------------- |
| macOS              | sandbox-exec (Seatbelt)            | Spawn bash via `sandbox-exec -f profile.sb` | Claude Code, Codex, Gemini |
| Linux              | Landlock (`landlock` crate v0.4.4) | Library call, self-sandboxing, no root      | Codex CLI                  |
| Linux (alt)        | bubblewrap                         | Shell out to `bwrap`                        | Claude Code                |
| Linux (complement) | seccomp                            | Syscall filtering                           | Codex CLI                  |

**Recommended for ion**: Landlock on Linux (kernel-native, no external deps), Seatbelt on macOS. Sandbox bash child processes, not ion itself.

## Config Pattern Comparison

**Claude Code** (deny > ask > allow, glob):

```json
{
  "permissions": {
    "allow": ["Bash(npm run *)", "Read"],
    "deny": ["Bash(rm -rf *)"]
  }
}
```

**Amp** (ordered rules, first-match-wins, regex):

```json
{
  "amp.permissions": [
    {
      "tool": "Bash",
      "matches": { "cmd": "/^git (status|log)$/" },
      "action": "allow"
    },
    { "tool": "*", "action": "ask" }
  ]
}
```

**opencode** (per-tool, last-match-wins, glob):

```json
{ "permission": { "bash": { "*": "ask", "git *": "allow", "rm *": "deny" } } }
```

**Droid** (tiered, no config):

```
--auto low    → file edits, formatters
--auto medium → installs, git commit, curl
--auto high   → git push, deploys
```

## References

- [UpGuard: YOLO Mode Hidden Risks](https://www.upguard.com/blog/yolo-mode-hidden-risks-in-claude-code-permissions)
- [Anthropic: Beyond Permission Prompts](https://www.anthropic.com/engineering/claude-code-sandboxing)
- [Landlock Rust crate](https://docs.rs/landlock)
- [Claude Code permissions docs](https://code.claude.com/docs/en/permissions)
- [Codex CLI security](https://developers.openai.com/codex/security/)
- [Amp permissions](https://ampcode.com/news/tool-level-permissions)
- [opencode permissions](https://opencode.ai/docs/permissions/)
- [Droid CLI reference](https://docs.factory.ai/reference/cli-reference)
