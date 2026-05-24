# Tools

Ion registers a small native tool universe:

```text
bash, edit, find, grep, ls, read, write
```

Normal coding mode exposes the Pi-style active subset to the provider:

```text
bash, edit, read, write
```

The read-only discovery tools `find`, `grep`, and `ls` are available through
read/all tool modes, but they are not active by default. This keeps normal
coding turns close to Pi while preserving typed discovery when a user selects
that mode.

Memory, MCP, subagent, model-visible compaction, and rewind/checkpoint control
surfaces are deferred or hidden from the default tool surface. `/compact`
remains a host command because context survival is reliability work. Skill
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
current registered and active tool names. Sandbox execution is parked while
the native core loop stabilizes; the default bash tool runs foreground commands
directly in the workspace.

Background jobs are deferred. The native Pi-parity tool path only runs
foreground commands; `/jobs` and `/stop` stay hidden until async process UX is
designed as a coherent later feature.

Native `bash` accepts `command` plus an optional per-command `timeout` in
seconds. There is no default timeout; normal turns are bounded by user/provider
cancellation and explicit tool/provider timeouts rather than a hidden whole-turn
deadline.

Native Pi-parity execution is trusted by default. Approval tiers, persistent
policy files, and sandbox permission UX are deferred until the core loop and
TUI are stable.

Native `read` returns model-visible text file contents with line numbers. For
supported images (`png`, `jpeg`, `gif`, `webp`), it returns a text note plus an
image content part through Canto so vision-capable providers can inspect the
image. The TUI still compacts read rows by default, but the model receives
stable line references for follow-up edits.

File and search tool path inputs follow Pi-style normalization for common
model output: leading `@` is stripped, Unicode space variants are normalized to
ASCII spaces, and local `file://` URLs are accepted. Non-local `file://host/...`
URLs are rejected.

Native `grep` and `find` remain dedicated read-only tools instead of being
collapsed into `bash`. They use ripgrep (`rg`) semantics for ignore handling:
ignore files are respected, hidden files are included when useful for coding
work, and `.git` internals are excluded. Ion does not auto-download `rg`; a
future in-process engine such as ripgo should be evaluated with benchmarks
before replacing the battle-tested ripgrep baseline.

Large model-visible tool results are truncated with explicit continuation or
omission markers. If the model needs the omitted content, it should rerun the
tool with a narrower command, path, or line range.

Native `write` remains separate from targeted edits. Native `edit` accepts an
`edits` array for one or more exact replacements in a single file. It validates
every replacement against the original file content, rejects overlapping edits,
checkpoints once, writes one temporary file, and finalizes with one rename.
Cross-file changes should be emitted as separate serialized tool calls.

Ion keeps its model-visible tool wrappers rather than directly exposing
Canto's stable `coding` package tools. Canto remains the framework substrate;
Ion's wrappers own product-level names, line-numbered reads, ripgrep search,
checkpoints, compact TUI display, and edit error messages tuned for
coding-agent recovery.

The same boundary applies to Canto's skill primitives. Canto can provide a
validated skill registry plus `read_skill` and `manage_skill` primitives, but
Ion owns install UX, trust policy, prompt exposure, and whether those tools are
model-visible at all. Ion's current `/skills` command is read-only discovery,
not activation.

Native `write` and `edit` create pre-change checkpoints before they mutate
files. Checkpoints are kept as recovery metadata, but `/rewind` polish is
deferred and should not be treated as part of the default model-visible tool
surface.

Structured edits require exact `oldText` matches inside `edits[]`, paired with
the replacement `newText`. Ambiguous edit failures include line numbers. LF
snippets copied from `read` can still match CRLF/BOM files without changing the
file's line-ending style.
