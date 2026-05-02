# Tools

The default model-visible native tool surface is deliberately small:

```text
bash, read, write, edit, multi_edit, list, grep, glob
```

Memory, MCP, subagent, model-visible compaction, and rewind/checkpoint control
surfaces are deferred or hidden from the default tool surface.
`/compact` remains a host command because context survival is reliability work.
Skill tools are also deferred: future `read_skill` and `manage_skill` support
must be enabled through an explicit skills surface rather than the default
coding toolset. The host-side `/skills [query]` command can list installed
local skill metadata without injecting those skills into the model prompt.

Ion uses Canto's lazy tool loading. When the registered tool surface is larger
than the lazy threshold, the model initially sees `search_tools` plus any eager
core tools. It can call `search_tools` to unlock specific hidden tool schemas.

Use:

```text
/tools
```

to show the registered tool count, whether lazy loading is active, and the
current tool names. The startup banner and `/tools` both report the active bash
sandbox posture for the native Canto backend.

Bash sandboxing is configured with:

```text
ION_SANDBOX=off|auto|seatbelt|bubblewrap
```

Explicit `seatbelt` and `bubblewrap` modes fail closed when their backend is
unavailable. `auto` uses the platform backend when present and reports when it
falls back to `off`.

Background bash is planned as a same-tool extension rather than new default
tools: `bash` will eventually support foreground runs, background job starts,
job output reads, and job kills with the same policy and sandbox posture. Until
that lands, long-running commands should still be run carefully with explicit
timeouts or in a user-managed terminal.

Approval tiers remain deliberately small:

| Mode | Behavior |
|---|---|
| READ | read tools allowed; write/execute blocked; sensitive asks |
| EDIT | read tools allowed; write/execute/sensitive follow policy |
| AUTO | all tools allowed |

Granular persistent rules live in `~/.ion/policy.yaml`; see
`docs/security/policy.md`.

Native `read` returns model-visible file contents with line numbers. The TUI
still compacts read rows by default, but the model receives stable line
references for follow-up edits.

Native `grep` and `glob` remain dedicated read-only tools instead of being
collapsed into `bash`. They use ripgrep (`rg`) semantics for ignore handling:
ignore files are respected, hidden files are included when useful for coding
work, and `.git` internals are excluded. Ion does not auto-download `rg`; a
future in-process engine such as ripgo should be evaluated with benchmarks
before replacing the battle-tested ripgrep baseline.

Large model-visible tool results are truncated with an explicit marker that
includes the cutoff and omitted byte count. If the model needs the omitted
content, it should rerun the tool with a narrower command, path, or line range.

Native `write`, `edit`, and `multi_edit` remain separate. A merged
Pi-style `edit(edits[])` surface is a future candidate, but it should only
replace the split after a local edit eval proves equal or better reliability.

Ion keeps its model-visible tool wrappers rather than directly exposing
Canto's stable `coding` package tools. Canto remains the framework substrate;
Ion's wrappers own product-level names, line-numbered reads, ripgrep search,
checkpoints, sandbox/status integration, compact TUI display, and edit error
messages tuned for coding-agent recovery.

The same boundary applies to Canto's skill primitives. Canto can provide a
validated skill registry plus `read_skill` and `manage_skill` primitives, but
Ion owns install UX, trust policy, prompt exposure, and whether those tools are
model-visible at all. Ion's current `/skills` command is read-only discovery,
not activation.

Native `write`, `edit`, and `multi_edit` create pre-change checkpoints before
they mutate files. Checkpoints are kept as recovery metadata, but `/rewind`
polish is deferred and should not be treated as part of the default
model-visible tool surface.

Structured edits require exact `old_string` matches. Use
`expected_replacements` with broad replacements so Ion can fail before writing
when the file contains a different number of matches. Ambiguous edit failures
include line numbers. LF snippets copied from `read` can still match CRLF/BOM
files without changing the file's line-ending style.
