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
current registered and active tool names. Shell execution defaults to trusted
local execution with the launching user's permissions and environment. Set
`ION_SANDBOX=seatbelt` or `ION_SANDBOX=bubblewrap` to select an enforceable OS
boundary explicitly; `auto` selects an available backend and fails clearly
when none exists. `ION_SANDBOX=off` is the explicit spelling for the same
unrestricted boundary.

Shell environment inheritance is controlled by `tool_env` in
`~/.ion/config.toml`. The default is `inherit`, matching trusted local
execution. Select `allowlist` or `inherit_without_provider_keys` when the
operator wants to reduce credential or host-variable exposure. MCP stdio
servers still start from a minimal allowlist and receive only their explicitly
configured overrides.

Skill tools are runtime-configured rather than silently enabled. Set
`skill_tools = "read"` to register and expose `read_skill`; `tool_mode =
"all"` also includes it in the complete tool surface. With the default skill
mode, `/skills` and `ion skill list` remain host-side discovery only. Global
skills are read from `~/.ion/skills`; project skills in `.ion/skills` are
loaded only for a trusted project.

Project-local instructions (`AGENTS.md`/`CLAUDE.md`), prompt templates in
`.ion/prompts`, and skills in `.ion/skills` are untrusted until explicitly
trusted. Pass `--trust` once from the project directory to persist trust for
that directory and its descendants. Without trust, including in print mode,
Ion skips those project-local resources while still loading global resources.
Trusted project content supplies context only; it cannot grant trust, change
runtime policy, or bypass the selected confirmation or sandbox mode.

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

External actions use the durable runtime action boundary. The default
policy is trusted local execution; set `trust_mode = "confirm"` explicitly or
pass `--trust-mode confirm` to require interactive confirmation. Project trust
(`--trust`) controls resource loading and is separate from execution policy.
Confirmation and sandboxing are separate controls. Requirement-bearing tools
are durably prepared and bound
to their normalized operation fingerprint before the TUI offers allow, allow
for the rest of the runtime, or deny. Denying produces a recoverable error
result without running the tool. Print mode and shutdown fail closed because
they cannot host a decision prompt. Actions that are started but cannot be
finalized recover as indeterminate and are never silently retried.

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

When an explicit sandbox mode is selected, MCP server sandboxes are read-only
and network-denied by default. Grant only the capabilities that the server
requires:

```toml
[[mcp_servers]]
name = "workspace"
command = "example-mcp"
writable_paths = ["build", "tmp"]
read_paths = ["vendor"]
allow_network = true
```

MCP servers can read their configured directory and the minimal runtime paths
needed by the selected sandbox. `read_paths` grants additional existing paths
relative to the server directory. `writable_paths` are existing paths relative
to that directory; `protected_paths` remain read-only even when a writable root
covers them. `allow_network` is an explicit per-server capability. In trusted
`ION_SANDBOX=off` mode, the OS boundary is intentionally unrestricted; an
explicit sandbox mode still rejects the MCP runtime when its backend cannot
be enforced.

Configured servers connect and discover tools atomically during runtime
startup. External tools are exposed under `mcp_<server>_<tool>` names, are
active in normal tool modes, and remain outside session persistence. Ion
validates remote names/descriptions, applies the enforced per-server sandbox
and workspace file policy, and rejects malformed servers, discovery failures, and tool-name
collisions instead of materializing a partial runtime. Every MCP call is an
approval requirement in `confirm` mode, including tools that do not expose a
file path. Server subprocesses run through the host sandbox and close with the
harness during cancellation, shutdown, and runtime switching. The action
fingerprint records the enforced per-server network capability, writable path
roots, and the names of explicitly configured server environment variables;
their values never enter the model or journal.

If Ion restarts after an external action began but its result was not durable,
print mode fails closed and the TUI exposes the action through `/actions`.
Reconcile it only with explicit operator evidence:

```text
/actions reconcile <action-id> completed <evidence>
ion actions --json list
ion actions --json reconcile <action-id> completed <evidence>
```

The CLI recovery command is provider-independent and emits redacted action
views; it never retries the external operation.

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
and relies on the runtime action boundary for preimage capture, serialization,
atomic replacement, and result verification. Cross-file changes should be
emitted as separate serialized tool calls.

Ion keeps its model-visible tool wrappers as the native coding-tool boundary.
Those wrappers own product-level names, line-numbered reads, ripgrep search,
checkpoints, compact TUI display, and edit error messages tuned for coding-agent
recovery.

The same boundary applies to Ion's skill primitives. Ion uses a validated
skill registry plus `read_skill`; it owns prompt exposure and whether skill
tools are model-visible at all. Project-local instructions, prompts, and
skills are loaded only after the host resolves persisted project trust; future
local extensions have no dynamic loader. Without trust, repository content is
not a security authority. Ion's current `/skills` command is read-only
discovery, not activation.

`/rewind` is a separate explicit checkpoint capability; writes and edits do not
create checkpoints as a hidden side effect. Use `/rewind` to list recent
checkpoints, then `/rewind <checkpoint-id>` to preview the paths that would
change. Restoration requires the explicit `/rewind <checkpoint-id> --apply`
command. Rewind changes files only; it does not erase or rewrite session
history, and it refuses checkpoints from another workspace.

Structured edits require exact `old_string` matches inside `edits[]`, paired
with `new_string`. `replace_all` and `expected_replacements` control repeated
matches. Ambiguous edit failures include line numbers. LF snippets copied from
`read` can still match CRLF/BOM files without changing the file's line-ending
style.
