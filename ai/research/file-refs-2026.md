# @file and @folder Inline References in Coding Agents

Research for tk-i2o1. Last updated: 2026-02-07.

## Tool Comparison Summary

| Tool         | Syntax                      | File Types                     | Images              | PDFs                 | Folders                    | Size Limits                         | Truncation                  | Context Protection                 |
| ------------ | --------------------------- | ------------------------------ | ------------------- | -------------------- | -------------------------- | ----------------------------------- | --------------------------- | ---------------------------------- |
| Claude Code  | `@path/file`                | All text, images, PDFs, .ipynb | Yes (visual)        | Yes (page-by-page)   | Directory listing only     | 2000 lines default, 2000 chars/line | Line-based offset/limit     | Autocompact at ~83.5% of window    |
| Cursor       | `@file`, `@folder`          | All workspace files            | Yes                 | Via docs provider    | Yes (recursive)            | Auto-condensed                      | Automatic summarization     | Condensation to fit context        |
| Aider        | `/add file`, CLI args       | Text, code, images             | Yes (vision models) | No native support    | Glob patterns              | Repo map for context                | No built-in truncation      | Manual `/drop`, repo map tokens    |
| Gemini CLI   | `@path/file`, `@dir/`       | Text only (excludes binary)    | No                  | No (binary excluded) | Yes (recursive, git-aware) | 1MB per file default                | Skip files over limit       | File count limits, focused queries |
| Continue.dev | `@File`, `@Folder`, `@Code` | Workspace files                | No (IDE-dependent)  | No                   | Yes (`@Folder`)            | Error on large files                | None documented             | Model context limit check          |
| Zed AI       | `@file`, `@dir`, `@symbol`  | Text, code, images (paste)     | Yes (clipboard)     | No                   | Yes                        | No documented limit                 | Silent discard if too large | Token count display, new thread    |

## Claude Code -- Detailed Analysis

### @file Reference System

Users type `@path/to/file` in the CLI input. Tab completion is available. Drag-and-drop also works. Multiple files can be referenced in a single message.

### Read Tool Internals

The Read tool is the core mechanism for file content injection:

- **Default line limit:** 2000 lines from beginning of file
- **Line truncation:** 2000 characters per line (lines beyond this are cut)
- **Output format:** `cat -n` style (line numbers + tab + content)
- **Optional params:** `offset` (start line), `limit` (number of lines)
- **Paths:** Absolute only, no directories

### File Type Handling

| Type                         | Handling                                                                                                                            |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Text/code                    | Read as-is with line numbers                                                                                                        |
| Images (PNG, JPG, GIF, WebP) | Presented visually via multimodal                                                                                                   |
| PDFs                         | Page-by-page extraction (text + visual). Optional `pages` param for large PDFs (>10 pages must specify range, max 20 pages/request) |
| Jupyter (.ipynb)             | All cells with outputs, code + text + visualizations                                                                                |
| Binary                       | Error returned                                                                                                                      |
| Empty files                  | System warning message                                                                                                              |

### Context Protection

- Autocompact triggers at ~83.5% of context window (~167K of 200K)
- Reserve buffer of ~33K tokens for summarization workspace
- `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` env var adjusts trigger point
- Manual `/compact` available for proactive management
- Line number formatting adds ~70% token overhead (known issue #20223)

### Key Design Choices

1. Read tool is a separate agentic tool call, not inline injection -- the model decides when and what to read
2. For `@file` references in chat input, content is fetched and injected before sending to model
3. Directory references produce listings, not recursive content dumps
4. Large PDFs require explicit page ranges to prevent context blowout

## Cursor -- Detailed Analysis

### @ Mention System

Cursor provides four categories accessible via `@`:

1. **@Files & Folders** -- Reference entire files or folders for context
2. **@Code** -- Select specific code snippets (functions, classes)
3. **@Docs** -- Built-in and custom documentation sites
4. **@Past Chats** -- Reference previous conversation threads

### File Handling

- Large files and folders are "automatically condensed to fit within context limits"
- Condensation is a summarization process, not simple truncation
- The system uses "dynamic context discovery" (2026 blog post) -- providing fewer details upfront, letting the agent pull context as needed
- Semantic search is used for intelligent file discovery
- Files are filtered by `.cursorignore` rules

### Context Protection

- Automatic condensation/summarization for large content
- Dynamic context discovery reduces upfront token usage
- Token-efficient approach: only necessary data pulled in
- No documented hard file size limits -- relies on intelligent summarization

## Aider -- Detailed Analysis

### File System

- **`/add file`** -- Add files to chat for editing
- **`/add file --read-only`** or **`--read`** flag -- Add as reference only
- **`/drop file`** -- Remove files to free context
- **Glob patterns:** `/add src/*.py` supported
- **Images:** `/add screenshot.png`, `/paste` for clipboard, or CLI arg
- **URLs:** `/web https://url` to scrape web content
- **Launch args:** `aider file1.py file2.js`

### Supported Image Formats

JPEG, PNG, GIF, BMP -- works with vision-capable models (GPT-4o, Claude Sonnet). Token cost calculated from image dimensions.

### Context Management Strategy

- **Repo map:** Tree-sitter-based map of the entire git repo sent as context. Uses ~1024 tokens by default. Provides awareness of codebase structure without including full file contents.
- **Philosophy:** "Don't add lots of files to the chat" -- user is responsible for curating context
- **No built-in truncation:** Files are sent in full. If they exceed the model's context, behavior degrades silently (model may ignore content).
- **Manual management:** User must `/drop` files to free space. No auto-summarization.
- Token counting displayed in prompt

### Key Limitation

No native PDF support. No automatic chunking for large files. When a file exceeds context, the model may act as if it has no context (Ollama issue #2901). The burden is entirely on the user to manage context size.

## Gemini CLI -- Detailed Analysis

### @ Reference System

- **`@file.ts`** -- Include single file
- **`@src/`** -- Include directory contents (recursive)
- **`@src/api/`** -- Focused directory reference
- Parses `@` syntax, resolves paths, invokes `read_many_files` tool, injects into prompt

### File Filtering

- **Binary exclusion:** JPG, PDF, EXE etc. auto-excluded (`excludeBinaryFiles: true` in settings)
- **Git-aware:** Respects `.gitignore`, excludes `node_modules/`, `dist/`, `.git/`, `.env`, `.log`
- **Configurable:** Settings in `.gemini/settings.json`

### Size Limits

- **Per-file:** Default 1,048,576 bytes (1MB) via `maxFileSizeBytes`
- **Files over limit:** Silently skipped
- **Directory references:** May hit file count limits on large repos
- **Recommendation:** Use focused queries over broad directory refs

### Context Management

- `/compress` command replaces file contents with summaries
- `/context` command shows currently loaded files
- 1M token context window with Gemini models
- Token consumption warning: "File contents consume context tokens"

### Key Design: Text-Only

Gemini CLI explicitly does not support images or PDFs via `@` references. This is a deliberate simplicity choice for a code-focused tool.

## Continue.dev -- Detailed Analysis

### Context Provider System

Continue uses a plugin architecture for context providers:

| Provider          | Description                           |
| ----------------- | ------------------------------------- |
| `@File`           | Reference any workspace file          |
| `@Code`           | Reference specific functions/classes  |
| `@Folder`         | Reference code from a specific folder |
| `@Git Diff`       | Current branch changes                |
| `@Current File`   | Active editor file                    |
| `@Terminal`       | Last command + output                 |
| `@Open`           | All open files or pinned files        |
| `@Clipboard`      | Recent clipboard items                |
| `@Tree`           | Workspace structure overview          |
| `@Docs`           | Documentation sites                   |
| `@Web`            | Web page content                      |
| `@Codebase`       | RAG-based codebase snippets           |
| `@Search`         | Code search results                   |
| `@Url`            | Content from a URL                    |
| `@Repository Map` | Codebase outline with signatures      |
| `@OS`             | Platform/architecture info            |
| `@HTTP`           | Custom POST to external servers       |

### File Handling Issues

- Known bug: files >12K or >400 lines triggered error "exceeds the allowed context length" even when model supports 128K (issue #5291, fixed)
- No documented PDF or image support through `@File`
- Context providers are extensible via MCP servers
- No documented truncation or chunking strategy

## Zed AI -- Detailed Analysis

### @ Mention Types

In both Agent Panel and Inline Assistant:

- `@file` -- Reference files
- `@directory` -- Reference directories
- `@symbol` -- Reference code symbols
- `@thread` -- Reference previous conversation threads
- `@rules` -- Reference rules files
- `@diagnostics` -- Reference current problems

### Image Support

Images supported via clipboard paste in the Agent Panel message editor. Not via `@` file reference.

### File Type Handling

Issue #35297 revealed that non-standard file extensions (like `.map` compiler output) were silently excluded from context -- the filename was included but not the content. This suggests Zed has an allowlist or heuristic for which file types get content included.

### Context Protection

- Token count displayed as user types
- Recommendation to create new thread when approaching context window
- No documented automatic truncation or summarization
- Large files may be silently discarded (no error message)

## PDF Text Extraction -- Rust Crates

### Comparison Table

| Crate           | Approach           | Dependencies     | Text Quality | Performance         | Maintenance                   | License    |
| --------------- | ------------------ | ---------------- | ------------ | ------------------- | ----------------------------- | ---------- |
| `pdf-extract`   | Pure Rust          | None (pure Rust) | Moderate     | Moderate            | Active (v0.10.0, Oct 2025)    | MIT        |
| `lopdf`         | Pure Rust          | None (pure Rust) | Basic        | Slow for large PDFs | Active (v0.39.0)              | MIT        |
| `pdfium-render` | FFI (Pdfium C++)   | Pdfium binary    | Excellent    | Fast                | Active (v0.8.37, Nov 2025)    | MIT/Apache |
| `pdfium` (new)  | FFI (Pdfium C++)   | Pdfium binary    | Excellent    | Fast                | New (text extraction planned) | GPL-3.0    |
| `poppler-rs`    | FFI (poppler-glib) | poppler, glib    | Excellent    | Fast                | Active (v0.25.0)              | MIT        |
| `poppler`       | FFI (poppler C)    | poppler lib      | Good         | Fast                | Stable (v0.6.0)               | GPL-2.0    |

### Analysis

**`pdf-extract`** -- Best pure-Rust option for text extraction. Simple API: `extract_text_from_mem(&bytes)`. No external dependencies. Handles most standard PDFs well. Struggles with complex layouts, scanned documents, and some encodings. 625K downloads total, actively maintained.

**`lopdf`** -- Low-level PDF manipulation library. Has `extract_text()` but it's basic and can be slow on large documents. Better suited for PDF creation/editing than text extraction. 2K GitHub stars, well-maintained.

**`pdfium-render`** -- Wraps Google's Pdfium (used in Chrome). Best text extraction quality, handles complex layouts, forms, annotations. Requires distributing or linking the Pdfium binary (~15-20MB). Dynamic linking at runtime. 583 stars, actively maintained. Not thread-safe (Pdfium limitation).

**`poppler-rs` / `poppler`** -- Binds to poppler, the same library behind `pdftotext`. Excellent extraction quality, fast performance. Requires poppler system library (`brew install poppler` on macOS, `apt install libpoppler-glib-dev` on Linux). The `poppler-rs` crate is GNOME-maintained. The `poppler` crate is simpler but GPL-2.0 licensed.

### Recommendation for ion

**Primary: `pdf-extract`** for zero-dependency PDF text extraction. It's pure Rust, MIT-licensed, and handles common PDFs well enough for context injection.

**Fallback: `pdfium-render`** if higher quality is needed, but it adds significant distribution complexity (bundling or requiring Pdfium binary).

**Avoid: `poppler-rs`** for a CLI tool -- the glib dependency chain is heavy and GPL license is restrictive.

## Document Conversion -- Non-PDF Formats

### Format Handling Strategies

| Format           | Approach                               | Rust Crate(s)                           |
| ---------------- | -------------------------------------- | --------------------------------------- |
| CSV/TSV          | Read as text, optionally parse headers | `csv` (BurntSushi)                      |
| HTML             | Convert to markdown or plain text      | `fast_html2md`, `html2text`, `lol_html` |
| JSON             | Read as text (already structured)      | Built-in serde_json                     |
| YAML/TOML        | Read as text                           | Built-in                                |
| Word (DOCX)      | XML extraction from zip                | `docx-rs` or shell out to `pandoc`      |
| Markdown         | Read as-is                             | N/A                                     |
| Jupyter (.ipynb) | Parse JSON, extract cell contents      | Custom JSON parsing                     |
| Images           | Pass as binary for vision models       | Read bytes, base64 encode               |
| Binary/compiled  | Reject with error                      | Detect via magic bytes or extension     |

### `extractous` -- Swiss Army Knife

The `extractous` crate (by yobix-ai) wraps Apache Tika as native binaries via GraalVM. Supports PDF, DOCX, PPTX, XLSX, HTML, XML, CSV, RTF, and more. However, it bundles Java-compiled native libraries (~61KB JAR), making it heavyweight for a CLI tool.

### Practical Approach for a CLI Agent

For a code-focused CLI tool, the 80/20 approach:

1. **Text files** (code, config, markdown, CSV, JSON, YAML, TOML, XML, HTML): Read as-is
2. **Images** (PNG, JPG, GIF, WebP): Base64-encode for vision models
3. **PDFs**: Use `pdf-extract` for text extraction
4. **Everything else**: Reject with clear error message listing supported types

Advanced format support (DOCX, XLSX, etc.) adds complexity with minimal benefit for a coding agent. Users can convert to text/markdown externally.

## Large File Handling -- Best Practices

### Strategies Used Across Tools

| Strategy                 | Used By      | Description                             |
| ------------------------ | ------------ | --------------------------------------- |
| Line-based truncation    | Claude Code  | First N lines, with offset/limit params |
| Byte-size limit          | Gemini CLI   | Skip files over 1MB                     |
| Automatic summarization  | Cursor       | Condense content to fit context         |
| User-managed             | Aider        | User must `/add` and `/drop` manually   |
| Silent discard           | Zed          | Drop content silently if too large      |
| Token counting pre-check | Continue.dev | Error if file exceeds model context     |

### Recommended Strategy for ion

**Multi-layer approach:**

1. **Extension-based detection**: Classify files by type (text, image, PDF, binary). Reject unsupported binary types immediately.

2. **Size gate**: Check file size before reading.
   - Text files: Warn if >100KB, truncate at configurable limit (default 200KB or ~50K tokens)
   - PDFs: Warn if >20 pages, require explicit page range for >10 pages
   - Images: Warn if >10MB (vision model limits)

3. **Head-first truncation**: For text files exceeding limit, include first N lines with a clear marker: `[... truncated at line N of M total lines]`. This preserves file header, imports, and structure.

4. **Token estimation before injection**: Count approximate tokens (chars/4) before adding to context. If the file would consume >25% of remaining context, warn the user.

5. **User control**: Let users specify line ranges with `@file:10-50` syntax for precise inclusion.

## Context Window Protection

### Key Principles

1. **Never let a single @file blow the context.** Hard cap at percentage of remaining context (e.g., 50%).
2. **Warn before injecting large content.** Show estimated token count and ask for confirmation above threshold.
3. **Prefer head truncation over silent discard.** Partial content with a truncation marker is more useful than nothing.
4. **Display running token count.** Users need visibility into context consumption.
5. **Make limits configurable.** Power users need control over truncation thresholds.

### Proposed Token Budget System

```
Total context window: N tokens
System prompt: ~S tokens
Remaining budget: N - S tokens
Per-file cap: min(remaining * 0.5, configurable_max)

If file_tokens > per_file_cap:
  Truncate to per_file_cap with marker
  Warn user: "Truncated @file.rs to ~{tokens} tokens ({lines} lines). Use @file.rs:1-100 for specific range."
```

## Recommended Approach for ion

### Syntax Design

```
@file:path/to/file.rs          -- Include file contents
@file:path/to/file.rs:10-50    -- Include lines 10-50
@folder:src/                    -- Include directory tree listing
@image:screenshot.png           -- Include image (existing, keep as-is)
```

**Rationale for `@file:` prefix** (vs bare `@path`):

- Avoids ambiguity with other @ references (e.g., `@tool:`, `@image:`)
- Explicit prefix makes intent clear in autocomplete
- Consistent with ion's existing `@image:` pattern

### Implementation Priorities

1. **Phase 1: Text files** -- Read and inject with line-based truncation, token estimation, range syntax
2. **Phase 2: Directory listings** -- Tree output with file sizes, not recursive content dump
3. **Phase 3: PDF extraction** -- `pdf-extract` crate, page-range support
4. **Phase 4: Image passthrough** -- Already exists as `@image:`, unify UX

### File Type Detection

Use a combination of:

- File extension mapping (primary, fast)
- Magic bytes check for ambiguous extensions (fallback)
- Reject known binary extensions: exe, dll, so, dylib, o, a, class, jar, zip, tar, gz, etc.

### Autocomplete

- On typing `@file:`, show fuzzy file finder (respect .gitignore)
- On typing `@folder:`, show directory completions
- Show file size and estimated token count in completion menu
- Sort by recency (recently modified files first, matching Glob tool behavior)

## Sources

- Claude Code tools reference: https://gist.github.com/bgauryy/0cdb9aa337d01ae5bd0c803943aa36bd
- Claude Code context buffer: https://claudefa.st/blog/guide/mechanics/context-buffer-management
- Cursor @ Mentions docs: https://cursor.com/docs/context/mentions
- Cursor dynamic context discovery: https://cursor.com/blog/dynamic-context-discovery
- Aider tips: https://aider.chat/docs/usage/tips.html
- Aider images/URLs: https://aider.chat/docs/usage/images-urls.html
- Gemini CLI file references: https://deepwiki.com/google-gemini/gemini-cli/3.3-file-references-and-context
- Continue.dev context providers: https://docs.continue.dev/customize/deep-dives/custom-providers
- Zed Agent Panel docs: https://zed.dev/docs/ai/agent-panel
- Zed Inline Assistant docs: https://zed.dev/docs/ai/inline-assistant
- pdf-extract: https://github.com/jrmuizel/pdf-extract
- pdfium-render: https://github.com/ajrcarey/pdfium-render
- lopdf: https://github.com/J-F-Liu/lopdf
- poppler-rs: https://crates.io/crates/poppler-rs
- extractous: https://github.com/yobix-ai/extractous
- html2text: https://lib.rs/crates/html2text
