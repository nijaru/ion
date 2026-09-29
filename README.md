# Ion

Ion is an unreleased Rust coding agent for a local working directory. It has
one coding loop for terminal chat, headless prompts and Rust library hosts. The
agent can read, edit and write files and run a native shell command. Sessions
are saved in SQLite and continue across launches. Ion is usable for short
coding tasks; its session and model controls are still simpler than Pi's.

## Start

Build with the checked-in Rust 1.98.0 toolchain:

```sh
cargo build --locked -p ion
target/debug/ion models
```

Use `target/debug/ion` in the commands below, or install the binary on your
`PATH` with `cargo install --locked --path crates/ion-app`.

Ion picks a catalog model automatically when its provider key is present in
`DEEPSEEK_API_KEY`, `XIAOMI_API_KEY`, `OPENROUTER_API_KEY`, `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`. The default preference starts with DeepSeek Flash, then
MiMo Flash, then DeepSeek Flash through OpenRouter. To select one explicitly,
run `ion use PROVIDER MODEL` with the exact ID shown by `ion models`.
`ion login PROVIDER` accepts a key at a masked terminal prompt when no
environment key is available. `ion auth` shows which credential source is
active, and `ion logout PROVIDER` removes a saved key. Environment keys take
precedence.
With the existing Pi FNOX profile, for example:

```sh
fnox --profile pi exec -- target/debug/ion --provider openrouter --model deepseek/deepseek-v4.1-flash
```

From the project directory:

```sh
ion                        # terminal chat
ion run 'Inspect and fix the failing test'
ion -p 'Summarize the changes'
ion --json run 'Inspect and fix the failing test' > events.jsonl
ion rpc                    # persistent JSONL control on stdin/stdout
git diff | ion -p 'Review this change'
ion --continue             # reopen the latest session in this directory
ion sessions               # list saved sessions and their IDs
ion --session ID inspect   # committed history for a selected session as JSON
ion --continue clone       # copy the latest conversation to a new session
ion --continue turns       # list Turn numbers and prompt previews
ion --continue fork 2      # new session before Turn 2, preserving the source
ion --continue fork 2 --after # new session after settled Turn 2
ion --continue compact     # summarize old context; retain the raw Session
```

`--cwd PATH` chooses a working directory. By default, a run starts a new
session; `--continue` reopens the most recently active one in that directory.
`--session ID` selects a listed session, and `--session PATH` can create or
open an exact SQLite path for scripts. Opening and quitting an empty chat does
not displace the latest conversation in `--continue` or `sessions`. An
existing session uses its recorded directory, and an explicit `--cwd` must
match it. Headless runs print the
session ID to stderr. In the TUI, `/new`, `/clone`, `/fork`, `/fork-after TURN`, `/resume`, `/name`, `/session`,
`/model` and `/compact` manage the conversation; `/login PROVIDER` and `/logout PROVIDER`
manage saved keys. The TUI model picker searches catalog and configured
custom routes. A resumed session restores its model; `ion use` sets the
default for new sessions.
Cloning copies committed conversation and context into a new session with
independent future turns. Both sessions still use the same live working
directory; cloning does not copy or restore files.
`/fork` opens a searchable Turn picker and restores the selected user input
to the editor in a new Session; `/fork TURN` selects directly. `/fork-after
TURN` continues after that Turn's recorded end. CLI `fork` prints the new
Session ID. The source retains all later history. These operations copy
conversation facts, not working files, and an unfinished Turn cannot be an
after-Turn point. RPC clients can use `list_turns` and `fork` with `turn` and
optional `after: true`.
This unreleased branch uses Session format 2; earlier development Session
files are not reopened.
For headless integrations, `--json` works with `run PROMPT` or `--print PROMPT`.
Headless prompts prepend UTF-8 data piped through stdin, up to 8 MiB. The
selected model's context window and Ion's encoded request bound can reject
large input even when it fits that raw input limit.
Plain text mode writes the committed final answer to stdout after a successful
turn. Tool progress and errors go to stderr; a failed turn does not print a
provisional answer as if it had completed.
In JSONL mode, stdout contains one JSON object per line: a `session` record with the ID and
directory, ordered `text_delta`, tool lifecycle, recovery and final records,
then a `run_end` record with `completed`, `cancelled` or `failed` status.
`tool_started` and `tool_finished` share a `call_id`; `tool_rejected` reports
a call that was never dispatched because the model response was truncated.
`response_restarted` means earlier text deltas from that incomplete attempt
were replaced after context compaction; consumers should discard those deltas.
Diagnostics stay on stderr, and failure also sets a nonzero exit status. The `final` record is
the committed assistant answer; earlier text deltas are for live display.

`ion rpc` keeps a Session open for a subprocess client. It emits a `ready`
record, then accepts one LF-terminated JSON command per stdin line. Commands
can carry a string `id`; each response repeats it. For example, send
`{"id":"1","type":"prompt","message":"Inspect this project"}`. A successful
prompt response includes a Turn ID and confirms that the input entered the
Session. Keep reading progress records with that Turn ID until `turn_end`
reports `completed`, `cancelled` or `failed`; a response alone is not the
answer. `final` is the committed answer. Other commands are `steer`, `abort`,
`get_state`, `inspect`, `list_sessions`, `list_turns`, `list_models`,
`list_resources`, `reload_resources`, `set_model`, `new_session`, `fork`,
`switch_session` and `set_name`.
`prompt` also accepts `images` as an array of local paths, relative to the
Session's working directory. `steer` queues text for the active Turn; `abort` requests
cancellation. Session, resource and model changes require an idle Turn.
Malformed commands receive a failed response, and commands over 1 MiB are
rejected. Stdout is reserved for protocol records, stderr for diagnostics.
Closing stdin cancels a running Turn and waits for its terminal record.
While a turn runs, the editor remains available: Enter steers the next model
step, Alt-Enter queues a separate follow-up turn, Alt-Up returns the most
recent queued follow-up to the editor, and Ctrl-C cancels. Up and Down browse
earlier prompts when the cursor reaches the first or last editor line. Type
`@` to pick a project file, or use Tab after a partial `@path`; the picker
inserts a path reference for the model to read, not the file's contents.
Ctrl-O opens the latest complete tool result; `/tools` lists results and
`/tool N` opens a selected one. Esc or Ctrl-O closes the result view. Input
that has not reached the model returns to the editor if the turn fails or is
cancelled.
In the terminal, `!command` runs a shell command in the live working
directory and includes its observed result in later model context.
`!!command` runs it without sharing the result with the model. Both commands
remain visible in the saved Session. Ctrl-C requests cancellation of a
running command; a command that started can still have external effects.

Ion loads `AGENTS.md` instructions found along the working directory's
ancestor path. A nested linked worktree's copy shadows the main checkout's
copy of the same file. `ion use` also accepts a custom model with
`--endpoint URL --wire chat-completions` or `--wire anthropic-messages`.
For a llama.cpp server whose model emits unreplayable reasoning, use
`--wire llama-cpp-no-thinking` to disable it on each request. Custom remote
endpoints require HTTPS and a key supplied through
`ION_CUSTOM_API_KEY` or `--api-key-env NAME`. Loopback HTTP, including
`localhost`, can run without a key.

Attach JPEG, PNG, GIF or WebP files with `ion --image PATH run "PROMPT"` or
`ion --image PATH chat`; repeat `--image` for several images. In chat,
`/image PATH` attaches a file to the next prompt. Relative paths resolve in
the Session's working directory. Ion decodes and checks the file, applies
image orientation and resizes large images before accepting the Turn. Source
files are limited to 32 MiB, and each inline image to 5 MiB within the
current 8 MiB request bound. A resize note gives the model the sent dimensions.
Image bytes are stored in the Session so follow-up requests can still see
them after the source file changes; `ion inspect` shows an image marker
instead of printing base64. A custom endpoint needs `ion use ... --images`
to declare that its model accepts image input. Clipboard image paste is not
yet a TUI input path; save the image to a file and attach it.

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
`/skills`, `/prompts` and `/reload`. Personal resources take precedence over
same-named project resources; nearer project directories take precedence over
ancestors. Invalid resources are skipped with a diagnostic. Project resources
are treated as lower-trust repository text under the same live-directory tool
permissions as `AGENTS.md`; review unfamiliar resources before using them.

Add local MCP tool servers explicitly with `ion mcp add NAME COMMAND [ARGS...]`;
`ion mcp list` and `ion mcp remove NAME` manage the saved user configuration.
Ion starts configured servers when a coding client starts, discovers their
tools, and exposes each as `mcp__NAME__TOOL` alongside read, edit, write and
exec. Headless, TUI and RPC use the same tool set. Server processes inherit
the user's environment and permissions and run in the Session's working
directory. A repository file does not launch an MCP server merely because
Ion opened that directory. A failing server is reported at startup; a failed
tool call becomes an error result visible to the model. Current MCP support
handles text and structured tool results; image/audio tool results report an
explicit unsupported-content error. For Rust embedders, `Host::agent_with_tools`
composes a custom `CodingToolHost` with built-ins; a same-name custom tool
replaces that one built-in.

Tools act directly in the working directory with the host user's permissions.
There is no implicit sandbox. If a process stops during a tool call, Ion
records its effect as unknown when the next prompt begins; it does not rerun
the call automatically. `ion --continue inspect` reads the existing log
without making that repair. `exec` retains the final 64 KiB observed from each
output stream. It reports omitted bytes when capture completes and marks a
capture incomplete if an inherited pipe remains open after output goes idle.
Commands use Bash when available, then fall back to POSIX sh. Commands have
no default timeout; pass
`timeout_ms` when a deadline is needed. `read` uses byte offsets and returns a
UTF-8-safe `next_offset`; `edit` accepts ordinary LF or CRLF text and preserves
the file's BOM and unaffected line endings. A damaged Session file is skipped
by `sessions` and `--continue`, while opening its exact path reports the
error. Ion summarizes settled history when its request
nears a known model's context window or exceeds its transport bound, and can
retry one model request after a provider reports context overflow. The raw
conversation remains inspectable; `compact` and `/compact` also trigger this
explicitly. Longer saved histories are summarized in bounded steps when one
summary request cannot fit. A single oversized prompt or tool result may
still exceed the context limit when no settled group can be summarized.
Transient provider failures can trigger up to two cancellable retries before
stream output; retry events appear in the TUI and JSONL output. A response that
stops after producing partial output is not replayed silently.
If an output-token limit cuts off identifiable tool calls, Ion records them
as skipped errors and lets the model reissue complete calls. No tool from the
truncated response runs.
When observed output use is below the selected model's ceiling, Ion first makes one
compact-and-retry attempt if a settled history prefix is available. It drops
the incomplete attempt and reports the restart to streaming clients.
Coding requests use the catalog model's output ceiling, reduced when the
current context leaves less estimated room; there is no separate 16k app cap.
A completed response with no answer or tool call fails the Turn; it does not
save an empty assistant message that would break later provider replay.

## Current limits

Ion has completed live coding repairs with DeepSeek V4.1 Flash, MiMo V2.6
Flash, OpenRouter DeepSeek V4.1 Flash, and a custom llama.cpp endpoint reached
through a loopback tunnel on macOS. OpenRouter DeepSeek V4.1 Flash also
completed a live read/edit/shell repair and cross-process resume in an
unprivileged Linux ARM64 container. On native Fedora, a custom local
llama.cpp Qwen endpoint completed a read/edit/shell repair and cross-process
resume. The code and session results were checked independently; these are
short-task samples, not broad model or platform parity.

The TUI clips tool output in its default view; the full stored result is
available through Ctrl-O or `/tool`. Context pressure currently
uses a rough request-size token estimate; custom routes without a known
context window use only the transport bound. Direct
OpenAI returned a no-credits HTTP 429 in the available account; Anthropic was
not qualified. [ARCHITECTURE.md](ARCHITECTURE.md)
holds the design contracts.

Repository checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
scripts/smoke.sh
python3 scripts/smoke_terminal.py
```

The smoke checks use an offline local model stream. See [AGENTS.md](AGENTS.md)
for maintainer instructions.

For Rust embedding, `ion-host::Host` composes the same model catalog,
credentials, project instructions and Session discovery used by the CLI.
`ion-core::CodingAgent` and `CodingSession` own the coding loop and committed
conversation; `Host::agent_with_tools` accepts a custom `CodingToolHost`.
Ion does not yet provide long-lived subprocess control or resource loading
through this host API.

## License

[MIT](LICENSE)
