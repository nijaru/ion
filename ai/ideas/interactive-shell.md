# Interactive Shell Support

**Date**: 2026-01-19
**Status**: Idea (post-MVP)
**Reference**: https://github.com/nicobailon/pi-interactive-shell

## Problem

Current bash tool runs commands and waits for completion. Interactive prompts (y/n, passwords, editors) cause hangs or require user intervention.

## Solution: PTY-Based Interactive Mode

### Architecture (from pi-interactive-shell)

```
Agent → PTY wrapper → subprocess
           ↓
    Terminal emulator (captures state)
           ↓
    TUI overlay (user can observe/takeover)
```

### Key Features

| Feature             | Description                        |
| ------------------- | ---------------------------------- |
| Full PTY            | Subprocess sees real terminal      |
| Agent input         | Send y/n, ctrl+c, arrows, text     |
| User takeover       | User can type to assume control    |
| Session persistence | Reattach to backgrounded processes |

### Implementation Levels

**Level 1 (Basic)**: Detect interactive prompt, show agent terminal state, let agent send y/n/text.

**Level 2 (Medium)**: Named key support (ctrl+c, arrows), timeout detection for hung processes.

**Level 3 (Full)**: Complete PTY emulation with xterm rendering, session persistence, user takeover.

## Rust Considerations

| Component          | Crate                           |
| ------------------ | ------------------------------- |
| PTY                | `portable-pty` or `pty-process` |
| Terminal emulation | `vt100` or `alacritty_terminal` |
| Async              | Already using tokio             |

## Use Cases

- `npm init` / `cargo init` prompts
- Git interactive rebase
- Database CLIs (psql, mysql)
- Editors (when agent needs to use vim/nano)
- Package manager confirmations

## Not MVP

Current bash tool is sufficient for most coding tasks. Interactive support adds complexity and is better suited for post-MVP polish.
