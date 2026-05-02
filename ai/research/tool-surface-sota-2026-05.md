---
date: 2026-05-02
summary: Reference comparison for Ion tool shape, search/list tools, permissions, and sandbox priority
status: active
---

# Tool Surface Reference Pass

**Status:** reference, 2026-05-02
**Scope:** native Ion tool surface, edit shape, search/list tools, permissions, and sandbox priority.

## Recommendation

Keep Ion's P1 default tool surface stable while the native loop is under stabilization:

- `bash`
- `read`
- `write`
- `edit`
- `multi_edit`
- `list`
- `grep`
- `glob`

Do not add new default tools during P1. Do not collapse search/list into shell. Do not move normal editing into Python, `sed`, heredocs, or ad hoc shell patches.

The one tool-shape question worth revisiting after P1 is complete: merge targeted editing into a single Pi-style `edit` tool with an `edits[]` array, while keeping `write` separate for create/overwrite.

## Edit and write

Reference patterns:

- Pi uses a single structured `edit` tool with `edits[]`, where every replacement is matched against the original file. It keeps `write` separate.
- Claude/managed-agent docs expose `read`, `write`, and exact string-replacement `edit` as separate built-ins.
- OpenCode exposes `edit`, `write`, `patch`, and `multiedit`, but all file modifications are governed by the same `edit` permission.
- Codex uses an `apply_patch` grammar that can create, update, move, and delete files, but it is a heavier model-facing syntax and has a dedicated parser.

Direction for Ion:

- Keep `write` separate. Whole-file create/overwrite is a different risk and display shape than targeted replacement.
- Keep structured edit tools as the only recommended normal edit path.
- Do not adopt a Codex-style patch grammar during stabilization. It is powerful, but it adds parser and model-format failure modes for Ion's mixed-provider target.
- After P1, consider replacing `edit` + `multi_edit` with one `edit` schema:

```json
{
  "file_path": "path/to/file.go",
  "edits": [
    {
      "old_string": "exact unique text",
      "new_string": "replacement text",
      "expected_replacements": 1
    }
  ]
}
```

The merged tool should preserve Ion's already-hardened behavior: CRLF/BOM-safe matching, line-numbered ambiguity/count errors, atomic validation before write, explicit replacement counts, and no partial writes.

## Grep, glob, and list

Keep dedicated read-only search/list tools in Ion.

Why:

- Policy is cleaner. `grep`, `glob`, and `list` can be auto-allowed as read-only without giving the model arbitrary shell execution.
- Display is cleaner. `Grep(pattern)`, `Glob(pattern)`, and `List(path)` preserve the model's intent in transcript/replay better than `Bash(rg ...)`.
- Truncation and result markers are controlled by Ion, not shell output conventions.
- Path containment, cancellation, replay titles, and model-visible result limits are easier to enforce in typed tools.
- OpenCode and Claude managed-agent docs both keep dedicated grep/glob-style tools; Pi prefers grep/find/ls tools over bash when they are available; Codex keeps structured read/list/search-style paths in its core.

Shell remains the escape hatch for advanced repo-specific commands. The prompt should not forbid `rg`, `fd`, `find`, or `ast-grep` in `bash`; it should simply prefer dedicated tools for ordinary file discovery.

Keep `list`. Directory inspection is not the same interaction as glob search: it is lower-intent, easier to scan, and maps naturally to read-only permission/display.

## Search engine

Current stabilization choice:

- Keep ripgrep semantics as the near-term baseline.
- Do not auto-download `rg`/`fd` at runtime.
- Defer ripgo integration until a measured task proves semantic parity and latency on real repos.

Measured `tk-03hf` checkpoint, 2026-05-02:

- Built local ripgo release from `/Users/nick/github/nijaru/ripgo` to
  `/tmp/ripgo-bench/ripgo`.
- Semantic fixture: visible file, hidden file, ignored `*.log`, nested file,
  and `.git/config`, all containing `needle`.
- Ion's current grep command:
  `rg --max-count 100 --heading --line-number --color never --hidden --no-require-git --glob '!.git/**'`.
  It returned visible, hidden, and nested matches; it excluded ignored files and
  `.git/config`.
- Closest ripgo CLI command:
  `/tmp/ripgo-bench/ripgo --max-count 100 --heading --line-number --color never --hidden`.
  It respected the ignored `*.log`, but included `.git/config`. Adding
  `--glob-not '.git/**'` or `--glob-not '**/.git/**'` did not fix this fixture.
- `rg --files --hidden --no-require-git --glob '!.git/**'` is still the
  current `glob` foundation. ripgo's CLI does not expose an equivalent file-list
  mode, so an Ion replacement would need direct package integration with
  `walk`/`ignore`, not a CLI swap.
- Hyperfine on Ion's `internal/` tree, warmed with 3 runs and measured with 10:

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `rg --max-count 100 --heading --line-number --color never --hidden --no-require-git --glob '!.git/**' -- 'func .*Execute' internal` | 9.3 ± 1.5 | 7.5 | 11.4 | 1.44 ± 0.26 |
| `/tmp/ripgo-bench/ripgo --max-count 100 --heading --line-number --color never --hidden 'func .*Execute' internal` | 6.5 ± 0.5 | 5.8 | 7.4 | 1.00 |

The measured search hit the same file set, but output ordering/spacing differs.
`go test ./...` passed in the ripgo repo.

Conclusion: keep current ripgrep-backed Ion tools for now. ripgo is fast enough
to keep as a serious future candidate, but it needs `.git` exclusion parity,
file-list/glob replacement coverage, cancellation/truncation integration, and
Ion-side tests before it replaces `rg`.

Future ripgo replacement criteria:

- `.gitignore`, `.ignore`, hidden-file, and `.git` exclusion semantics match current behavior.
- Cancellation works at least as well as the current path.
- Model-visible truncation markers remain explicit.
- Large-repo latency is competitive with the current ripgrep-backed implementation.
- Path containment and symlink escape behavior remain covered by tests.

Tree-sitter or `ast-grep` should not be a P1 built-in. They are excellent structural refactor tools, but they are not replacements for text search and file discovery. Expose them through `bash` or a later optional extension if real usage demands it.

## Permissions and sandbox

Pi's no-permissions posture is useful as a simplicity reference, but Ion should not copy it wholesale.

Recommended priority:

- P1: minimal native loop stays simple. Read-only tools are allowed; normal editing and command execution work without policy/trust code changing loop correctness.
- I3: restore a simple user-facing safety model: read/edit/auto, with auto only explicit.
- Later: keep sandbox orthogonal and visible. It is worth having, but it should not block core-loop stabilization.

Why keep sandbox work:

- Claude Code, Codex, and OpenHands all invest in sandboxed command execution or controlled environments.
- Sandbox protects the host from shell commands and subprocesses, which typed file tools alone cannot cover.
- It should remain a runtime boundary, not a replacement for good tool schemas or clear permissions.

## Sources

- Local Pi source: `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/core/system-prompt.ts`, `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/core/tools/edit.ts`, `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/core/tools/grep.ts`, `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/core/tools/find.ts`.
- Local Codex source: `/Users/nick/github/openai/codex/codex-rs/apply-patch/src/parser.rs`, `/Users/nick/github/openai/codex/codex-rs/core`, `/Users/nick/github/openai/codex/codex-rs/tui`.
- Claude managed-agent tools: <https://platform.claude.com/docs/en/managed-agents/tools>
- Claude Code settings, permissions, and sandbox: <https://code.claude.com/docs/en/settings>
- OpenCode built-in tools: <https://open-code.ai/en/docs/tools>
- OpenHands sandbox overview: <https://docs.openhands.dev/openhands/usage/sandboxes/overview>
- OpenAI apply_patch guide: <https://platform.openai.com/docs/guides/tools-apply-patch>
