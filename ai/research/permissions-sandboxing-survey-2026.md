# Permissions & Sandboxing in Coding Agents (February 2026)

**Research Date**: 2026-02-06
**Purpose**: Compare permission models, approval systems, and OS-level sandboxing across major coding agents; evaluate practical sandboxing options for a Rust CLI tool.

---

## Executive Summary

| Agent           | Permission Modes                                                           | Approval Granularity                            | OS Sandbox                                 | Network Isolation            | Approval Persistence                                                         |
| --------------- | -------------------------------------------------------------------------- | ----------------------------------------------- | ------------------------------------------ | ---------------------------- | ---------------------------------------------------------------------------- |
| **Claude Code** | 6 modes (default, acceptEdits, plan, delegate, dontAsk, bypassPermissions) | Per-tool with glob patterns, deny > ask > allow | macOS Seatbelt, Linux bubblewrap           | Proxy-based domain filtering | Bash: permanent per-project; Edits: session-only                             |
| **Codex CLI**   | 3 sandbox levels + 4 approval levels                                       | Per-sandbox-mode + per-approval-mode            | macOS Seatbelt, Linux Landlock+seccomp     | Disabled by default, opt-in  | Session-only                                                                 |
| **Gemini CLI**  | Default prompt + YOLO mode                                                 | Per-tool (allow once / allow always)            | macOS Seatbelt (5 profiles), Docker/Podman | Profile-dependent            | Session-only (always-approve bug: does not persist across terminal sessions) |
| **Aider**       | --yes flag (binary)                                                        | All-or-nothing                                  | None                                       | None                         | N/A (no approval system)                                                     |
| **Amp**         | allow/reject/ask/delegate per-tool                                         | Per-tool with built-in allowlist                | None                                       | None                         | Config-based (no session caching)                                            |

**Key finding**: Claude Code has the most sophisticated permission + sandboxing system. Codex CLI has the strongest OS-level sandbox (Landlock+seccomp on Linux). Gemini CLI offers the most sandbox profiles. Aider and Amp have no OS sandboxing.

---

## 1. Claude Code (Anthropic)

### Permission Modes

| Mode                | Behavior                                  |
| ------------------- | ----------------------------------------- |
| `default`           | Prompts on first use of each tool         |
| `acceptEdits`       | Auto-accepts file edits for session       |
| `plan`              | Read-only, no modifications or commands   |
| `delegate`          | Coordination-only for agent team leads    |
| `dontAsk`           | Auto-denies unless pre-approved via rules |
| `bypassPermissions` | Skips all prompts (containers/VMs only)   |

### Permission Rules

Rules follow `Tool` or `Tool(specifier)` syntax with deny > ask > allow precedence:

```json
{
  "permissions": {
    "allow": ["Bash(npm run *)", "Bash(git commit *)", "Read"],
    "deny": ["Bash(git push *)", "Read(./.env)"]
  }
}
```

- **Bash**: Glob wildcard patterns, shell-operator-aware (prevents `safe-cmd && evil-cmd` bypass)
- **Read/Edit**: gitignore-spec patterns with absolute (`//path`), home (`~/path`), relative (`/path` from settings, `./path` from cwd)
- **WebFetch**: Domain-based (`WebFetch(domain:example.com)`)
- **MCP**: Server/tool-level (`mcp__puppeteer__puppeteer_navigate`)
- **Task**: Per-subagent (`Task(Explore)`)

### Approval Persistence

| Tool Type          | "Yes, don't ask again"                        |
| ------------------ | --------------------------------------------- |
| Read-only          | No approval needed                            |
| Bash commands      | Permanently per project directory and command |
| File modifications | Until session end                             |

### OS-Level Sandboxing

**Architecture**: Filesystem isolation + network proxy, both enforced at OS level.

**Platforms**:

- **macOS**: Seatbelt (`sandbox-exec`) profiles
- **Linux/WSL2**: bubblewrap (`bwrap`) + socat

**Sandbox Modes**:

1. **Auto-allow**: Sandboxed commands run without approval; unsandboxable commands fall back to normal permission flow
2. **Regular permissions**: All commands go through standard flow even when sandboxed

**Filesystem**:

- Default writes: CWD and subdirectories
- Default reads: Entire computer except denied directories
- Configurable allow/deny paths

**Network**:

- Proxy server runs outside sandbox
- Domain allowlist with user confirmation for new domains
- Custom proxy support for enterprise (HTTPS inspection)

**Escape hatch**: `dangerouslyDisableSandbox` parameter falls back to normal permissions. Can be disabled with `allowUnsandboxedCommands: false`.

**Open source**: Sandbox runtime published as `@anthropic-ai/sandbox-runtime` npm package.

**Settings hierarchy**: Managed settings (admin) > CLI args > local project > shared project > user.

### Managed Settings (Enterprise)

Admins can deploy `/Library/Application Support/ClaudeCode/managed-settings.json` (macOS) or `/etc/claude-code/managed-settings.json` (Linux) with settings that cannot be overridden:

| Setting                           | Purpose                                  |
| --------------------------------- | ---------------------------------------- |
| `disableBypassPermissionsMode`    | Prevent users from bypassing permissions |
| `allowManagedPermissionRulesOnly` | Only managed rules apply                 |
| `allowManagedHooksOnly`           | Only managed hooks load                  |

---

## 2. Codex CLI (OpenAI)

### Sandbox Modes

| Mode                 | Filesystem   | Network            | Use Case            |
| -------------------- | ------------ | ------------------ | ------------------- |
| `read-only`          | Read only    | Blocked            | Safe browsing       |
| `workspace-write`    | CWD + /tmp   | Blocked by default | Default development |
| `danger-full-access` | Unrestricted | Unrestricted       | CI/containers only  |

### Approval System

`--ask-for-approval` / `-a` flag with four modes:

| Mode         | Behavior                                         |
| ------------ | ------------------------------------------------ |
| `on-request` | Prompts for out-of-workspace or network access   |
| `untrusted`  | Auto-approves edits, asks for untrusted commands |
| `never`      | Disables all prompts                             |
| `on-failure` | Prompts only on operation failure                |

**Presets**:

- `--full-auto` = `workspace-write` + `on-request` approvals
- `--dangerously-bypass-approvals-and-sandbox` = full access + no prompts (CI only)

### OS-Level Sandboxing

**macOS**: Seatbelt policies via `sandbox-exec` with mode-specific profiles.

**Linux**: Landlock + seccomp. This is the strongest sandbox implementation among all agents -- Landlock provides kernel-enforced filesystem restrictions without root, and seccomp filters syscalls.

**Windows**: Experimental native sandbox; WSL recommended.

### Network Isolation

Network defaults to **disabled**. Opt-in via config:

```toml
[sandbox_workspace_write]
network_access = true
```

Web search defaults to cached/pre-indexed results rather than live fetching to reduce prompt injection risk. Override with `web_search = "live"` or `"disabled"`.

### Approval Persistence

Session-only. The "Yes, and don't ask again for this command" option has reported bugs where it doesn't persist properly (GitHub issue #6395).

---

## 3. Gemini CLI (Google)

### Permission System

Default behavior: prompts for each tool action with three choices:

- **Yes, allow once**
- **Yes, allow always** (for this tool/command pattern)
- **No** (deny)

"Always approve" does **not** persist across terminal sessions (known issue #4340, open as of 2026).

### YOLO Mode

Enabled via:

1. `gemini --yolo` or `gemini -y` flag
2. `Ctrl+Y` runtime toggle
3. `"autoAccept": true` in `settings.json`

Auto-approves all tool actions (file writes, shell commands, etc.) without confirmation.

**Selective configuration**:

```json
{
  "autoAccept": true,
  "safeTools": ["read_file", "list_files"],
  "tools.shell.autoApprove": ["git ", "npm test"]
}
```

### OS-Level Sandboxing

**macOS Seatbelt** with 5 profiles (via `SEATBELT_PROFILE` env var):

| Profile                     | Writes                | Network   |
| --------------------------- | --------------------- | --------- |
| `permissive-open` (default) | Restricted to project | Allowed   |
| `permissive-closed`         | Restricted to project | Blocked   |
| `permissive-proxied`        | Restricted to project | Via proxy |
| `restrictive-open`          | Strict                | Allowed   |
| `restrictive-closed`        | Strict                | Blocked   |

**Docker/Podman** container sandbox (cross-platform):

- Complete process isolation
- Build from local Dockerfile or org registry
- `SANDBOX_FLAGS` env var for custom container flags

**Activation**:

- Flag: `-s` / `--sandbox`
- Env: `GEMINI_SANDBOX=true|docker|podman|sandbox-exec`
- Settings: `"tools": { "sandbox": "docker" }`

### Approval Persistence

Session-only. "Always" approval is session-scoped. Cross-session persistence is a requested but unimplemented feature.

---

## 4. Aider

### Permission Model

Aider has the simplest model -- essentially binary:

| Flag                 | Behavior                                  |
| -------------------- | ----------------------------------------- |
| (default)            | Shows edits, asks for confirmation        |
| `--yes`              | Auto-confirms all prompts                 |
| `--no-auto-commits`  | Disables auto git commits                 |
| `--no-dirty-commits` | Skips committing dirty files before edits |

No per-tool approval, no granular permissions, no sandbox. Git integration is the primary safety net -- every change is committed, and `/undo` reverts the last change.

**Requested but not implemented**: Granular `--yes` flags (`--yes-output`, `--yes-file`, `--yes-read`) per issue #1327.

### Sandboxing

None. Aider has no OS-level sandboxing, no filesystem restrictions, and no network isolation. Safety relies entirely on:

1. Git history (auto-commits make all changes reversible)
2. User review of proposed diffs before confirmation
3. The `--yes` flag being an explicit opt-in

---

## 5. Amp (Sourcegraph)

### Permission System

Amp uses a rule-based permission system with four actions:

| Action     | Behavior                     |
| ---------- | ---------------------------- |
| `allow`    | Execute without prompting    |
| `reject`   | Block the tool               |
| `ask`      | Prompt user for confirmation |
| `delegate` | Route to external program    |

**Built-in allowlist**: Common dev commands (`ls`, `git status`, `npm test`, `cargo build`) auto-approved. Destructive commands (`git push`, `rm -rf`) require confirmation.

View defaults: `amp permissions list --builtin`

### Sandboxing

None. No OS-level sandboxing, no filesystem isolation, no network restrictions.

**Known vulnerability (fixed July 2025)**: The AI could modify its own configuration to allowlist bash commands or add malicious MCP servers, enabling arbitrary code execution via prompt injection. This was a sandbox-escape-style attack through config file modification.

### Safety Features

- Secret redaction with `[REDACTED:amp]` markers
- Thread-level privacy controls
- Enterprise audit logging
- Zero data retention option

### Approval Persistence

Configuration-based only. No session caching of approvals. Rules in config files persist; runtime approvals do not.

---

## OS-Level Sandboxing Options for a Rust CLI

### macOS: Seatbelt / sandbox-exec

**Status**: Deprecated since macOS Sierra (2016) but still works and is actively used by Claude Code, Codex CLI, and Gemini CLI. No replacement until macOS 26 Containerization framework.

**How it works**: Write a Seatbelt profile (Scheme-like DSL), pass to `sandbox-exec -f profile.sb -- command`:

```scheme
(version 1)
(deny default)
(allow file-read* (subpath "/usr"))
(allow file-read* file-write* (subpath "/path/to/project"))
(allow network-outbound (remote tcp "localhost:*"))
(deny network*)
```

**From Rust**: Spawn child process via `Command::new("sandbox-exec")` with profile file.

**Existing Rust crate**: `keter-duty-rs` (9 stars, wrapper for sandbox-exec scripts). Low adoption -- most projects shell out directly.

**Practical considerations**:

- Works without root
- All child processes inherit sandbox
- Deprecated but no removal date announced
- Profile syntax is undocumented (reverse-engineered)
- Cannot sandbox the current process, only child processes

### macOS 26: Containerization Framework (WWDC 2025)

**Status**: New in macOS Tahoe (26), open source Swift framework.

**What it is**: NOT a replacement for sandbox-exec. Runs a lightweight Linux VM per container. Similar to Docker but native. Designed for Linux container workloads, not for sandboxing native macOS processes.

**Relevance to ion**: Low for near-term. This is for running Linux containers on Mac, not for restricting what a macOS CLI tool can do. sandbox-exec remains the practical choice.

### Linux: Landlock

**Status**: In mainline kernel since 5.13 (2021). Current version: ABI v5 with filesystem + TCP restrictions.

**How it works**: Unprivileged process restricts its own ambient rights via 3 syscalls. Once applied, restrictions cannot be removed for the process hierarchy.

**Rust crate**: `landlock` (v0.4.4, 175 stars, Apache-2.0/MIT)

- Official crate maintained by Landlock kernel maintainer
- MSRV: Rust 1.68
- Best-effort compatibility mode (graceful degradation on older kernels)

```rust
use landlock::{
    Access, AccessFs, PathBeneath, PathFd,
    Ruleset, RulesetAttr, RulesetCreatedAttr,
    ABI,
};

fn sandbox_to_cwd() -> Result<()> {
    let abi = ABI::V5;
    let cwd = PathFd::new(".")?;

    Ruleset::default()
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rule(PathBeneath::new(cwd, AccessFs::from_all(abi)))?
        .restrict_self()?;
    Ok(())
}
```

**Key properties**:

- No root required
- Self-sandboxing (restricts current process, not just children)
- One-way: cannot disable once enabled
- Nested sandboxes supported
- Orthogonal to namespaces

**Used by**: Codex CLI, Firejail, Cloud Hypervisor, systemd, and others (see landlock.io/integrations).

### Linux: seccomp

**Status**: Mature, in kernel since 3.5 (2012). Syscall filtering.

**How it works**: BPF filter on syscalls. Can allow, deny, trap, or log specific syscalls.

**Rust crate**: `seccompiler` (by AWS/Firecracker team) or `libseccomp` bindings.

**Practical use**: Complement to Landlock. Landlock handles filesystem/network access control; seccomp handles syscall-level restrictions (prevent `ptrace`, `mount`, etc.).

### Linux: bubblewrap (bwrap)

**Status**: Mature, used by Flatpak and Claude Code.

**How it works**: Creates unprivileged Linux namespaces with bind mounts. External tool, not a library.

**From Rust**: Shell out to `bwrap` binary.

**Comparison to Landlock**: bubblewrap provides broader isolation (PID namespace, mount namespace) but requires the binary to be installed. Landlock is kernel-native and can be used as a library.

### Linux: firejail

**Status**: Mature SUID sandbox. More heavyweight than Landlock/bubblewrap. Not suitable for library integration -- designed as a standalone tool.

---

## Recommendation for ion

### Near-Term (Practical)

**Permission system** (already designed and implemented in ion):

- Three modes (Read/Write/AGI) with CWD sandbox -- this is solid
- Per-command bash approval with session/permanent caching -- matches industry practice
- The existing design is comparable to Codex CLI's approach

**What to add**:

1. **Glob pattern matching for bash approvals** (like Claude Code): Allow users to configure `bash(npm *)` style rules instead of only exact command matching. This is the biggest UX improvement available.

2. **Config-file permission rules**: Allow `[permissions]` section in config with allow/deny lists, similar to Claude Code's `settings.json`. ion already has `config.toml` support.

3. **Safe tool defaults**: Ship a built-in allowlist of safe commands that don't need approval in Write mode (like Amp does). Examples: `git status`, `git diff`, `git log`, `cargo check`, `cargo test`, `ls`, `cat`, `find`, `grep`.

### Medium-Term (OS Sandboxing)

**Recommended approach**: Landlock on Linux, sandbox-exec on macOS.

| Platform         | Mechanism                     | Integration                              |
| ---------------- | ----------------------------- | ---------------------------------------- |
| Linux            | Landlock via `landlock` crate | Library call, self-sandboxing            |
| Linux (fallback) | bubblewrap for older kernels  | Shell out to `bwrap`                     |
| macOS            | sandbox-exec                  | Spawn bash via `sandbox-exec -f profile` |

**Architecture**: Sandbox the bash tool's child processes, not ion itself. This matches how Claude Code and Codex CLI work.

- Generate Seatbelt/Landlock rules dynamically based on CWD and `--no-sandbox` flag
- On Linux: Apply Landlock rules to restrict child process filesystem access to CWD
- On macOS: Generate Seatbelt profile and spawn bash through `sandbox-exec`
- Network: Default deny, with domain allowlist for known safe operations

**Why not bubblewrap on Linux**: Landlock is kernel-native (no external dependency), works as a library, and can self-sandbox. Codex CLI chose Landlock+seccomp over bubblewrap for these reasons. Claude Code uses bubblewrap because they're a Node.js project and shelling out is natural; for Rust, the native `landlock` crate is cleaner.

### What ion Already Has vs. What to Add

| Feature           | ion (current)                  | Industry Best                    |
| ----------------- | ------------------------------ | -------------------------------- |
| Permission modes  | Read/Write/AGI                 | 4-6 modes (Claude Code)          |
| Bash approval     | Per-command, session+permanent | Per-command with globs           |
| CWD restriction   | Checked in tool code           | OS-enforced (Landlock/Seatbelt)  |
| Network isolation | None                           | Proxy-based domain filtering     |
| Config rules      | Basic config.toml              | Deny > ask > allow with patterns |
| Managed settings  | None                           | Admin-deployed system-wide       |

**Priority ordering**:

1. P1: Glob pattern bash rules in config (high UX impact, low effort)
2. P1: Safe command allowlist (reduce approval fatigue)
3. P2: Landlock sandbox for Linux bash execution
4. P2: Seatbelt sandbox for macOS bash execution
5. P3: Network proxy for domain isolation
6. P3: Managed/enterprise settings

---

## References

- Claude Code sandboxing: https://code.claude.com/docs/en/sandboxing
- Claude Code permissions: https://code.claude.com/docs/en/permissions
- Codex CLI security: https://developers.openai.com/codex/security/
- Gemini CLI sandbox: https://google-gemini.github.io/gemini-cli/docs/cli/sandbox.html
- Amp security: https://ampcode.com/security
- Landlock Rust crate: https://github.com/landlock-lsm/rust-landlock (https://docs.rs/landlock)
- Landlock integrations: https://landlock.io/integrations/
- sandbox-exec deprecation discussion: https://news.ycombinator.com/item?id=44283454
- Apple Containerization (WWDC 2025): https://developer.apple.com/videos/play/wwdc2025/346/
- Anthropic sandbox blog: https://www.claude.com/blog/beyond-permission-prompts-making-claude-code-more-secure-and-autonomous
- Sandbox runtime (open source): https://github.com/anthropic-experimental/sandbox-runtime
