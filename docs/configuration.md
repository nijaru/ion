# Configuration and trust

Configure models, resources and executable contributions. Configuration and tool selection are not OS sandboxes.

[Start](../README.md) · [Terminal](terminal.md) · [Configuration](configuration.md) · [Tools](tools.md) · [Automation](automation.md) · [Rust embedding](embedding.md)

## Credentials and default model

Ion picks a catalog model automatically when its provider key is present in
`DEEPSEEK_API_KEY`, `XIAOMI_API_KEY`, `OPENROUTER_API_KEY`, `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`. The default preference starts with DeepSeek Flash, then
MiMo Flash, then DeepSeek Flash through OpenRouter. To select one explicitly,
run `ion use PROVIDER MODEL` with the exact ID shown by `ion models`.
`ion login PROVIDER` accepts a key at a masked terminal prompt when no
environment key is available. `ion auth` shows which credential source is
active, and `ion logout PROVIDER` removes a saved key. Environment keys take
precedence.

## Model tool selection

`--tools LIST` limits the model-visible tools to comma-separated exact names or
`*` patterns; `--exclude-tools LIST` removes matches after inclusion. Exclusions
win, and both apply to native tools, MCP tools and Code Mode's callable namespace.
`--read-only` is an allowlist preset for `read`, `ls`, `find`, `grep`, `history`,
MCP resource inspection and Code Mode when enabled; it excludes native mutations, direct shell
execution and managed children. These flags are model-loadout policies for the invocation, not
OS sandboxes or credential boundaries: user `!` shell commands and host-side
application effects remain separate. A managed child inherits the initiating
application's selection. `tool_search` is added only when the selection allows it
and selected deferred tools need discovery. `--read-only` cannot be combined with
`--tools` or `--exclude-tools`. Terminal coding also includes
`ask_user_question` when its question responder is configured; headless and RPC
coding do not declare that tool.

## Reasoning preference

Use `--reasoning high` for headless/chat/RPC startup or manual compaction,
`/reasoning high` in an idle terminal, or RPC
`{"id":"r","type":"set_reasoning","effort":"high"}`. Values are `default`,
`off`, `low`, `medium`, `high` and `budget:TOKENS`. The preference is
Session-owned: reopen and clone retain it, forks restore the selected Turn's
preference, and new Sessions start at `default`. Coding and compaction freeze
the same preference. Changes require no live operation. Reopened interrupted
history may remain unresolved: changing a future preference does not settle its
Turn, rewrite its original model/reasoning, or recover unfinished effects.
Logical-Turn continuation still requires matching authority and configuration. Unsupported adapter controls refuse before saving
the preference or making a model request. Model changes also preflight the
retained preference rather than silently resetting it. Explicit effort selection
saves the bound model and effort together, including an invocation-only model override.

Support is route-specific, not proof of remote model support or entitlement.
Messages currently accepts only `default`; llama.cpp's no-thinking route
accepts `default` and `off`, both disabling thinking. Other Chat routes accept
the five non-budget choices. DeepSeek maps medium/high to its high setting;
MiMo maps all three levels to enabled thinking. Every current HTTP route refuses
exact token budgets. Effort affects future generation, not retained historical
reasoning or signed continuation. `/settings thinking` remains display-only.

## Custom routes, provider continuation and capacity

`ion use` also accepts a custom model with
`--endpoint URL --wire chat-completions` or `--wire anthropic-messages`.
Custom provider identifiers contain only lowercase ASCII letters, digits and
hyphens, with 1–64 bytes; the same identifier is used for provider login.
`URL` may be a compatible API base such as `http://desktop:8080/v1` or the
complete `http://desktop:8080/v1/chat/completions` request URL. Ion appends
the standard wire path only when the URL is a base.
For a custom OpenRouter model, use `--wire openrouter-chat`. This route retains
plain reasoning or ordered structured `reasoning_details` across tool calls;
the generic `chat-completions` route does not assume that contract. Qualify a
new model with a tool-using turn before relying on it for coding.
When switching models between Turns, Ion keeps the saved transcript and tool
results but omits opaque reasoning from earlier model epochs in later model
requests. Switching back does not revive those older blocks. Model requests
now carry an explicit logical selection, effective physical provider/model and
route reason. Current direct routes use the same logical and effective model;
assistant, compaction and cache-warm usage facts retain the effective execution
identity and any provider-returned model identifier. Effective physical model
changes advance replay epochs independently of logical selection, so a future
virtual route cannot switch A → B → A and accidentally revive stale opaque
reasoning. Ion does not yet provide a virtual-model registry or router.
Anthropic encoding aliases unsigned migrated tool IDs when their format or a
collision requires it, keeping each call paired with its result. Aliases are
wire-only: committed IDs, other chat wires and retained native replay are
unchanged. Appending history does not rebind an earlier encoded prefix; a collision
that would require renaming retained replay reports an incompatibility instead.
OpenRouter requests include a persisted opaque conversation ID for best-effort
provider/cache affinity, independent of opening messages that resource reload or
compaction may change. Reopen retains it; clone/fork gets a fresh ID. Other
provider wires do not inherit OpenRouter's `session_id` field. Affinity does
not guarantee cache hits or pin an unavailable provider.
The catalog includes current Claude Fable 5.1, Opus 5.5 and Sonnet 5.5 on the
native Anthropic Messages route. Its signed thinking is retained across tool
and later Turn continuation. For reusable coding requests, these cataloged
native routes opt into Anthropic prompt caching and retain cache-read/cache-write
token counts separately from total input usage. Compatible tool-loadout changes
are encoded as native inline tool additions/removals/redefinitions so the
initial top-level tool prefix and signed-thinking prefix can remain stable;
instruction changes deliberately fall back to the latest leading context.
When a prefix cannot be preserved safely, Ion records the existing one-time
reasoning reset rather than altering signed history.

During a long active tool batch, current priced native Anthropic routes also
use economical streaming cache warming: shortly before the verified cache
lifetime expires, Ion may replay the exact last model request with a one-token
output ceiling when the expected avoided cache miss is at least $0.05. The
timer starts from that request's dispatch time, stops when the tool batch or
context advances, and is capped to one hour of active work. Policies without a
positive refresh point inside that hour—including cache lifetimes of ten seconds
or less—do not warm. Expired cache windows and elapsed active horizons never
restart warming after a delayed response. Refresh usage is stored as a cache_warm
Session accounting fact but never enters model context or normal transcript/export
output. Idle cache warming is not implemented.
Ion avoids compaction during a signed tool continuation; if its prefix changes
or cannot fit, the Turn fails without repeating a tool effect. Context-window
estimates fall back to neutral content counted once, without duplicate opaque
replay. Later requests can use total input usage from a compatible committed
assistant exchange to refine capacity estimates. Samples are Session-local and
require the same live model service, effective model, instructions, tools and
reasoning; images must match as well. Context/model changes, compaction, replay
reset, reopen and fork/clone discard the sample. Missing usage never means zero.
Known input plus its assistant's reported output establishes a context baseline;
only later messages are estimated. This avoids treating duplicate replay JSON as
provider input. Incomplete usage creates no new baseline; later opaque messages
without a known baseline use the fallback. Summary/cache-warm usage does not
calibrate normal requests. New image sets retain the conservative first-contact
image allowance. The independent encoded request bound still includes retained
replay. Neither estimate is an exact provider-token count or transport-wire limit.
Provider SSE frames are limited to 256 KiB. Newly retained decoded output,
including opaque replay, has an 8 MiB admission budget. Independent transport
bounds allow 64 MiB per response and 8 MiB without retained-state progress.
These are resource guards, not token limits: extreme fragmentation can still
reach the transport bound before the model's output allowance.
For a llama.cpp server whose model emits unreplayable reasoning, use
`--wire llama-cpp-no-thinking` to disable it on each request. Custom HTTP or
HTTPS endpoints can run without a key. Pass `--api-key-env NAME` or run
`ion login PROVIDER` when the endpoint needs one. An HTTP endpoint sends any
configured key in cleartext; use HTTPS when the endpoint offers it.

## Project instructions

Ion discovers `AGENTS.md` along the working directory's ancestor path.
Unreadable, invalid UTF-8 and oversized files produce source-specific resource
diagnostics instead of blocking startup; omissions are also marked in model
context. Files are included whole, up to 64 KiB each, within a 128 KiB project
instruction context including bounded omission notices. Closest files get budget
priority, then included files appear in ancestor-to-local order. A nested linked
worktree's copy shadows the main checkout's copy of the same file.

## Skills and prompt templates

Ion also discovers Agent Skills from `~/.agents/skills/`,
`~/.config/ion/skills/` (or `$XDG_CONFIG_HOME/ion/skills/`) and project
`.agents/skills/` directories from the working directory up to its Git root.
Each skill is a directory with `SKILL.md` and valid Agent Skills frontmatter.
Only its name, description and path enter the standing model instructions;
the model can read the full file when relevant. Use `/skill:NAME [request]` to
load it explicitly. Prompt templates are direct `.md` files in
`~/.config/ion/prompts/` or project `.ion/prompts/`; `/NAME [arguments]`
expands one before submitting it. Templates support `$1`, `$@`,
`${1:-default}` and `${@:N:L}` argument forms; quote an argument containing
spaces. `ion resources` lists both, and the TUI has
`/skills`, `/prompts` and `/reload`. Slash-command discovery uses this same
loaded inventory; `/reload` refreshes suggestions. Native terminal command names
take precedence over same-named templates in the terminal client. Starting,
cloning, forking or switching a
Session refreshes resources; `/reload` refreshes the current Session. Personal
resources take precedence over same-named project resources; nearer project
directories take precedence over ancestors. Invalid resources are skipped with
a diagnostic. Recognized resource arguments and expanded text must fit the
current request byte limit; expansion refuses oversize input instead of truncating
it. The terminal retains the literal draft and attachments; headless/RPC clients
receive an error before a Turn is accepted. Core still checks the complete
encoded message and attachments separately. Project resources are lower-trust repository text
under the same live-directory tool permissions as `AGENTS.md`; review
unfamiliar resources before using them.

## Named executable commands

Executable user commands are separate from skills and prompt templates. Put a
`commands.json` in the user configuration directory, or `.ion/commands.json` at
the nearest Git root (the working directory when there is no Git ancestor):

```json
{
  "commands": {
    "check": {
      "description": "Run the workspace tests",
      "program": "cargo",
      "args": ["test", "--locked"],
      "share_with_model": false
    }
  }
}
```

`share_with_model` is required. In the terminal, `/commands` lists enabled names,
descriptions and origins; `/command:check [arguments]` runs one. Discovery and Tab
completion do not execute it. Quotes and escapes group literal arguments, without
variable, glob or command substitution. Fixed manifest arguments precede user
arguments. Execution uses the Session's working directory and existing user-shell
admission, capture, cancellation and recovery. It records the actual launched
command and outcome, not a fabricated model tool call or user prompt. With sharing
false, both stay out of later model context; true shares them with the next model
request, without starting a request itself.

Project commands remain disabled until `ion commands trust` approves their current
manifest bytes and canonical root. `ion commands list` works without model setup;
`ion commands untrust` revokes only command trust, independently of `ion trust`
for MCP. Trusted project commands override user entries with the same name.
Manifests are strict JSON, limited to 1 MiB; duplicate command names are rejected. Editing a loaded source, deleting it
or revoking project trust rejects new launches; review, trust project changes and
`/reload` before running the new definition. Already admitted work keeps its
captured route. Reloading, listing or opening a repository never launches a command.

Commands use host permissions, not a sandbox: the manifest digest does not freeze
scripts or transitive executable code, and a program can interpret its own argv.
Read-only **model** tool selection does not revoke explicit user-shell/command
authority. Custom applications opt in through `Application::commands` (empty by
default) and user-shell authority; neither alone permits execution.
Commands cannot be queued as follow-ups. Headless/RPC prompt submission does not
execute terminal commands; embedded clients can explicitly use shared control's
`command` method. This is named execution, not arbitrary extension handlers,
lifecycle hooks or UI plugins.

## MCP servers and resources

Add local MCP tool servers explicitly with `ion mcp add NAME COMMAND [ARGS...]`.
Use `ion mcp add-http NAME URL` for a Streamable HTTP server; add
`--bearer-token-env VARIABLE` when it requires a bearer token already set in
the environment. Add `--exposure direct|deferred|hidden` to either command.
Project servers live in `<repository root>/.ion/mcp.json`; write them with
`ion mcp add --project ...` and remove them with `ion mcp remove --project NAME`.
Project configuration is inert until `ion trust` records its canonical project
root and current file digest. Editing the file invalidates trust; untrusted
entries are never connected. `ion mcp list` reports the project path and an
untrusted diagnostic. Trusted project entries override user entries with the
same name; `ion untrust` removes the trust record. Trust is an explicit launch
decision, not process isolation or a sandbox.
`direct` is the default and declares the server's tools. `deferred` keeps them
callable but out of the initial model loadout until `tool_search` activates a
match for a later request; Code Mode can also call them. `hidden` exposes no
model-visible tools and omits the server's resources from the MCP resource
tools. Hidden is model exposure, not process isolation or authority: the server
is still configured and inherits the host environment if started. Exposure
changes apply to future requests, never an already-issued request. `ion mcp list`
and `ion mcp remove NAME` manage the saved user configuration; concurrent
mutations serialize across Ion processes.
Ion starts configured servers when a coding client starts, discovers their
tools, and exposes each as `mcp__LENGTH_NAME__TOOL` alongside read, edit, write and
exec. `LENGTH` is the ASCII server-name length: `demo` uses `mcp__4_demo__TOOL`.
This delimiter keeps server/tool pairs distinct even when names contain `__`.
MCP tool names that need normalization or shortening receive a stable
hash suffix in the model-facing name; Ion calls the server with its original
tool name. Headless, TUI and RPC use the same tool set. Local server processes inherit
the user's environment and permissions and run in the Session's working
directory. A repository file does not launch an MCP server merely because
Ion opened that directory. A failing server is reported at startup while
healthy servers and the coding client remain available; a failed
tool call becomes an error result visible to the model. On controlled exit,
including failed startup or client preflight, Ion joins MCP services and HTTP
workers. It closes local server stdin, waits up to three seconds for the root
process to exit, then kills and reaps that root if needed. Service join and
local process cleanup errors are reported. The HTTP SDK still logs remote
session-deletion failures rather than returning them. Cleanup does not guarantee
termination of descendants or remote work, and abrupt process loss cannot
guarantee joined cleanup. Rust embedders must await `McpTools::shutdown` before
dropping their runtime. Cancelling that wait leaves the retained close running;
a later shutdown joins the same task instead of reporting an already-taken
connection as clean. Incoming messages are bounded before decoding: one stdio
JSON line or HTTP JSON/error body may be at most 64 MiB, and an SSE event stays
within the transport's 16 MiB default and that 64 MiB ceiling. Oversize input is
rejected rather than truncated into a different observation. A rejected stdio
line or SSE event retires that connection, so Ion cannot resume and replay it;
an oversize HTTP JSON/error body fails that request. Neither case reconnects and
repeats a possibly executed call, and lost transport errors surface after one
bounded retry instead of retrying forever. Local server
stderr goes to a private `0600` temporary file retaining the first 64 KiB while
the rest of the stream is drained so logging cannot block the protocol. Nonempty
captures appear as `server_stderr` metadata with the path, byte counts and any
capture error; startup diagnostics name servers that logged, and nothing is
written directly to the terminal. The capture stops and joins with the root
process, so descendants holding the pipe cannot stall shutdown.
Current MCP support delivers text, structured data and
normalized images. An audio block, embedded resource, resource link or invalid
image is omitted from delivery without discarding the rest: its index and the
validation reason appear under `conversion_notices` (bounded, with
`retained_blocks` and `omitted_blocks` counts). The server's own `isError` stays
the outcome flag, so a successful response is not rewritten as a failed call,
and no rejected image enters model context. When anything is omitted, the
original response is written to a private `ion-mcp-raw-result-*.json` named by
`raw_result_path`; a write failure reports `raw_result_error` instead. Resource
links are lower-trust metadata: Ion never fetches them or assumes a local path.
Servers that advertise resources also expose `list_mcp_resources`,
`list_mcp_resource_templates` and `read_mcp_resource`; these tools are added
when at least one configured server advertises resource capability.
Omitting `server` lists every resource-capable server with at most 64 pages per
server, while a `server` argument returns one page and an opaque `cursor`
continues it. MCP App UI resources (`ui://` or `mcp-app` profile HTML) are
omitted from listings. A read returns text and normalized images; text-like
blobs (`text/*`, JSON, `+json` or `+xml`) become text, and other binary content
is saved to a private temporary file whose path is returned. Resource
observations use the same bounded inline result and private full-output snapshot
as tool results. These operations are read-only and are never retried; a
cancellation that cannot flush within three seconds retires that connection.
Large text or structured results include a bounded preview and a
`full_output_path` to a private JSON file containing the complete result;
oversized structured content is omitted from the inline preview and marked
`structured_content_omitted`; `read` can inspect the full output in ranges.
Each invocation sends one `tools/call`; `input_required` continuations and task
results report an unsupported-result error rather than issuing hidden follow-up
calls or task polling. Cancellation notifies the original request and joins
local issuance. If that cannot flush within three seconds, Ion retires the
connection, joins its local work and removes its tools from later catalogs.
Other pending calls on that connection fail rather than being rerun; restarting
the client reconnects configured servers. Neither cancellation nor connection
closure proves that remote effects have stopped.
When a server announces a changed tool list, Ion refreshes that server before
the next model request. A failed refresh reports a warning and keeps its last
known list. Remote MCP OAuth login is not yet supported.
