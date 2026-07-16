# Tools

Ion registers a small native tool universe:

```text
bash, edit, find, grep, ls, read, write
```

Normal coding mode exposes the default active subset to the provider:

```text
bash, edit, read, write
```

The read-only discovery tools `find`, `grep`, and `ls` are registered but not
active by default. The always-active `search_tools` meta-tool searches the full
registry, activates matching names, and exposes them on the next provider
request. This keeps normal coding turns small while preserving typed discovery
when the model needs it.

Memory tools are opt-in through `memory_tools = "on"`; they are hidden from the
default model-visible surface otherwise. The host `/memory` command is always
explicit and operates outside session persistence. Subagent tools and
model-visible compaction remain deferred or hidden. `/rewind` is an explicit
host-only checkpoint recovery command described below.
MCP is an explicit runtime-only external-tool capability described below.
`/compact` remains a host command because context survival is reliability work.
Skill
tools are opt-in stdlib surfaces rather than default coding tools:
`read_skill` is available behind `skill_tools = "read"`, while `manage_skill`
remains deferred behind a later write-management design. The host-side
`/skills [query]` command can list installed local skill metadata without
injecting those skills into the model prompt.

Use:

```text
/tools
```

to show the registered tool count, whether lazy loading is active, and the
current registered and active tool names. Sandbox execution is selected by
`ION_SANDBOX`; the default is trusted local execution, while configured macOS
seatbelt or Linux bubblewrap modes fail closed when unavailable.

Shell environment inheritance is controlled by `tool_env` in
`~/.ion/config.toml`. The default `inherit` passes the process environment to
native Bash. `inherit_without_provider_keys` removes the provider credential
environment variables known to Ion, plus a configured `auth_env_var`, while
preserving normal development variables such as `PATH`.

Skill tools are runtime-configured rather than silently enabled. Set
`skill_tools = "read"` to register and expose `read_skill`; `tool_mode =
"all"` also includes it in the complete tool surface. With the default skill
mode, `/skills` and `ion skill list` remain host-side discovery only.

Background commands are managed by runtime-owned jobs. They are useful for dev
servers, watchers, and long builds without blocking the agent turn:

```json
{"command":"go test ./...","background":true}
{"action":"list"}
{"action":"output","job_id":"job-1","tail_lines":50}
{"action":"stop","job_id":"job-1"}
```

Jobs are process-group scoped, output-bounded, survive provider/model switches,
and are canceled during Ion shutdown. They are intentionally not persisted or
replayed after the process exits. The TUI exposes `/jobs` and `/jobs stop
<job-id>` for the same runtime state.

Native `bash` accepts `command` plus an optional per-command `timeout` in
seconds. There is no default timeout; normal turns are bounded by user/provider
cancellation and explicit tool/provider timeouts rather than a hidden whole-turn
deadline.

Long native `bash` output uses tail semantics: Ion keeps the last 2000 lines or
50KB for provider-visible context and writes the full truncated output to a temp
file referenced in the tool result. This preserves final test summaries and
compiler errors instead of returning only the command's head.

Native execution is trusted by default. For an interactive confirmation gate,
set `trust_mode = "confirm"` in `~/.ion/config.toml` or pass
`--trust-mode confirm`. Requirement-bearing mutating tools pause in the TUI
until the user chooses allow, allow for the rest of the runtime, or deny.
Denying produces a recoverable error result in the session rather than running
the tool. Print mode and shutdown fail closed because they cannot host a
decision prompt. Persistent approval policy is intentionally not part of the
runtime contract; future external tools must use the same broker before they
are exposed.

Provider requests retry transient failures with exponential backoff. Transport
failures retry until the active turn is canceled by default; provider-declared
rate-limit and server failures are bounded by `max_retries` (default `3`). Set
`retry_until_cancelled = false` for one-attempt behavior, or tune
`retry_base_delay_ms` (default `1000`, capped at `60000`) in
`~/.ion/config.toml`. The TUI shows the retry reason and countdown while it
waits. Retries happen before a stream is established; a partially consumed
stream is never replayed automatically.

Positive `max_session_cost` and `max_turn_cost` values are hard spend limits
for provider-reported usage. Ion blocks new prompts at the session limit and
cancels an active turn when a completed provider response crosses either
limit, clearing queued turns and showing the limit in the canceled status. If a
provider does not report cost, Ion cannot enforce a monetary limit for that
request.

## MCP servers

Ion can attach named MCP servers over stdio from `~/.ion/config.toml`:

```toml
[[mcp_servers]]
name = "workspace"
command = "npx"
args = ["-y", "@example/mcp-server"]
directory = "."

[mcp_servers.env]
EXAMPLE_TOKEN = "literal-value-for-the-server"
```

Configured servers connect and discover tools atomically during runtime
startup. External tools are exposed under `mcp_<server>_<tool>` names, are
active in normal tool modes, and remain outside session persistence. Ion
validates remote names/descriptions, applies the configured workspace file
policy, and rejects malformed servers, discovery failures, and tool-name
collisions instead of materializing a partial runtime. Every MCP call is an
approval requirement in `confirm` mode, including tools that do not expose a
file path. Server subprocesses close with the harness during cancellation,
shutdown, and runtime switching.

Native `read` returns model-visible text file contents with line numbers. For
supported images (`png`, `jpeg`, `gif`, `webp`), it returns a text note plus an
image content part through Ion's provider conversion so vision-capable providers
can inspect the image. The TUI still compacts read rows by default, but the
model receives stable line references for follow-up edits.

File and search tool path inputs normalize common terminal/model output: leading
`@` is stripped, Unicode space variants are normalized to ASCII spaces, and
local `file://` URLs are accepted. Non-local `file://host/...` URLs are rejected.
For `read`, Ion also retries common macOS filename variants when the normalized
path is missing: screenshot AM/PM narrow spaces, NFD Unicode filenames, and
straight apostrophes typed for curly apostrophes.

Native `grep` and `find` remain dedicated read-only tools instead of being
collapsed into `bash`. They use ripgrep (`rg`) semantics for ignore handling:
ignore files are respected, hidden files are included when useful for coding
work, and `.git` internals are excluded. Ion does not auto-download `rg`; a
future in-process engine such as ripgo should be evaluated with benchmarks
before replacing the battle-tested ripgrep baseline.

`grep` uses compact result shaping: `limit` counts matches, not rendered context
lines; `context` is formatted by Ion as compact before/after blocks;
long match and context lines are truncated to 500 characters with an explicit
notice; and directory/file results are displayed relative to the searched root.
`find` supports slash patterns even when `path` already narrows the search root,
while keeping output relative to that root.

Large model-visible tool results are truncated with explicit continuation or
omission markers. If the model needs the omitted content, it should rerun the
tool with a narrower command, path, or line range.

Native `write` remains separate from targeted edits. Native `edit` accepts an
`edits` array for one or more exact replacements in a single file. It validates
every replacement against the original file content, rejects overlapping edits,
checkpoints once, writes one temporary file, and finalizes with one rename.
Cross-file changes should be emitted as separate serialized tool calls.

Ion keeps its model-visible tool wrappers as the native coding-tool boundary.
Those wrappers own product-level names, line-numbered reads, ripgrep search,
checkpoints, compact TUI display, and edit error messages tuned for coding-agent
recovery.

The same boundary applies to Ion's skill primitives. Ion uses a validated skill
registry plus `read_skill`; it owns install UX, trust policy, prompt exposure,
and whether skill tools are model-visible at all. Ion's current `/skills`
command is read-only discovery, not activation.

Native `write` and `edit` create pre-change checkpoints before they mutate
files. Checkpoints are kept as recovery metadata and are scoped to the
canonical workspace. Use `/rewind` to list recent checkpoints, then
`/rewind <checkpoint-id>` to preview the paths that would change. Restoration
requires the explicit `/rewind <checkpoint-id> --apply` command. Rewind changes
files only; it does not erase or rewrite session history, and it refuses
checkpoints from another workspace.

Structured edits require exact `old_string` matches inside `edits[]`, paired
with `new_string`. `replace_all` and `expected_replacements` control repeated
matches. Ambiguous edit failures include line numbers. LF snippets copied from
`read` can still match CRLF/BOM files without changing the file's line-ending
style.
