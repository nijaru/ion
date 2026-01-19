# Edit Tool Patterns in Coding Agents

**Date**: 2026-01-19
**Sources**: Claude Code, OpenCode, Codex CLI, Pi-Mono, Amazon Kiro, Aider

---

## Summary

All major coding agents use **exact string replacement** as the primary edit mechanism. This is token-efficient, model-friendly, and deterministic. Line-number-based and diff-based approaches exist but are secondary.

---

## 1. Parameters Comparison

| Agent             | Parameters                              | Required | Optional                                |
| ----------------- | --------------------------------------- | -------- | --------------------------------------- |
| **Claude Code**   | `file_path`, `old_string`, `new_string` | All 3    | `replace_all` (default: false)          |
| **OpenCode**      | `oldString`, `newString`, `path`        | All 3    | None documented                         |
| **Codex CLI**     | `path`, `operation.diff` (V4A format)   | Both     | `operation.type` (create/update/delete) |
| **Pi-Mono**       | `filePath`, `old_string`, `new_string`  | All 3    | `replace_all`                           |
| **Kiro (Amazon)** | `path`, `oldStr`, `newStr`              | All 3    | None                                    |
| **Aider**         | SEARCH/REPLACE blocks in markdown       | N/A      | N/A (prompt-based)                      |

### Claude Code Schema (Canonical)

```json
{
  "name": "Edit",
  "input_schema": {
    "type": "object",
    "properties": {
      "file_path": {
        "type": "string",
        "description": "The absolute path to the file to modify"
      },
      "old_string": {
        "type": "string",
        "description": "The text to replace"
      },
      "new_string": {
        "type": "string",
        "description": "The text to replace it with (must be different from old_string)"
      },
      "replace_all": {
        "type": "boolean",
        "default": false,
        "description": "Replace all occurrences of old_string (default false)"
      }
    },
    "required": ["file_path", "old_string", "new_string"]
  }
}
```

---

## 2. Uniqueness Handling

All agents require uniqueness enforcement to prevent ambiguous edits.

### Strategies

| Strategy               | Agents Using                | Behavior                                 |
| ---------------------- | --------------------------- | ---------------------------------------- |
| **Fail if not unique** | Claude Code, OpenCode, Kiro | Error with count of matches              |
| **replace_all flag**   | Claude Code, Pi-Mono        | Opt-in to replace all occurrences        |
| **Context guidance**   | All                         | Prompt asks for more surrounding context |

### Claude Code Error Handling

```
Error: Text "old_string" appears 3 times. Use replaceAll: true or provide more context.
```

### OpenCode Error Handling

```
Error: oldString found multiple times and requires more code context to uniquely identify the intended match
```

### Best Practices from Kiro

```
CRITICAL REQUIREMENTS:
1. EXACT MATCHING: "oldStr" must match EXACTLY one or more consecutive lines
2. WHITESPACES: All whitespace must match exactly (spaces, tabs, line endings)
3. UNIQUENESS: Include sufficient context before and after (2-3 lines recommended)
4. REPLACEMENT: oldStr and newStr MUST BE DIFFERENT
```

---

## 3. Token Efficiency Approaches

### 3.1 Exact String Replacement (Most Common)

- **Token cost**: O(changed_content_size) - only sends changed portion
- **Used by**: Claude Code, OpenCode, Pi-Mono, Kiro
- **Advantage**: Minimal tokens, precise edits
- **Disadvantage**: Requires uniqueness, sensitive to whitespace

### 3.2 Unified Diff (V4A Format - Codex)

```
*** Begin Patch
*** Update File: path/to/file.py
@@ class BaseClass
@@     def search():
-        pass
+        raise NotImplementedError()
*** End Patch
```

- **Token cost**: O(changed_content_size + context)
- **Advantage**: Familiar format, handles multiple hunks
- **Disadvantage**: More tokens, parsing complexity

### 3.3 SEARCH/REPLACE Blocks (Aider)

```
README.md
<<<<<<< ORIGINAL
old content
=======
new content
>>>>>>> UPDATED
```

- **Token cost**: O(changed_content_size)
- **Advantage**: Readable, works with any model
- **Disadvantage**: Parsing required, conflict marker collisions

### Comparison Table

| Approach          | Tokens  | Precision | Model Compatibility |
| ----------------- | ------- | --------- | ------------------- |
| Exact string      | Lowest  | Highest   | All models          |
| V4A diff          | Medium  | High      | Trained models      |
| SEARCH/REPLACE    | Low     | High      | All models          |
| Full file rewrite | Highest | Perfect   | All models          |

---

## 4. Large File Handling

### 4.1 Read-Before-Edit Requirement

Claude Code enforces reading files before editing:

```javascript
// Validation
if (!readFileState.hasReadFile(file_path)) {
  throw new Error("You must use the Read tool to read the file before editing");
}
```

**Benefits**:

- Prevents stale edits
- Ensures model has current file content
- Reduces phantom edits (editing non-existent content)

### 4.2 Line-Limited Reading

All agents support offset/limit for large files:

```javascript
// Read with offset/limit
read(file_path, { offset: 100, limit: 50 }); // Lines 100-149
```

### 4.3 Modification Time Tracking

Claude Code tracks file mtime to detect external changes:

```javascript
if (file.mtime > readFileState.timestamp) {
  throw new Error("File was modified externally. Re-read before editing.");
}
```

### 4.4 MultiEdit Tool (Claude Code)

For multiple edits to same file, reduces round-trips:

```json
{
  "tool": "MultiEdit",
  "tool_input": {
    "file_path": "lib/feature.js",
    "edits": [
      { "old_string": "foo", "new_string": "bar" },
      { "old_string": "baz", "new_string": "qux" }
    ]
  }
}
```

---

## 5. Validation Flow (Claude Code - 9 Layers)

```
1. Parameter consistency (old_string != new_string)
2. Path normalization and permission check
3. File creation logic (empty old_string = new file)
4. New file permission check
5. File existence validation
6. Jupyter file type check (.ipynb blocked)
7. Force read validation (must read first)
8. File modification time check
9. String uniqueness check
```

---

## 6. Alternative Approaches

### 6.1 Line-Number Based (OpenHands)

```python
edit_file(
    path="/workspace/a.txt",
    start=1, end=3,
    content="REPLACE TEXT"
)
```

- **Advantage**: Precise, no uniqueness issues
- **Disadvantage**: Line drift, requires accurate line tracking

### 6.2 Full File Write (Simpler, Higher Token Cost)

Used as fallback or for new files:

```json
{
  "tool": "write",
  "path": "new_file.txt",
  "content": "entire file content..."
}
```

### 6.3 Computer Use Text Editor (Anthropic)

```javascript
textEditor_20241022({
  command: "str_replace" | "view" | "create" | "insert",
  path: string,
  old_str?: string,
  new_str?: string,
  insert_line?: number,
  view_range?: [number, number],
  file_text?: string
})
```

- Supports multiple commands: view, create, str_replace, insert
- More flexible but more complex schema

---

## 7. Recommendations for Ion

### Core Edit Tool

```rust
pub struct EditTool;

#[derive(Deserialize)]
pub struct EditInput {
    /// Absolute path to the file to modify
    pub file_path: String,
    /// Text to replace (must exist in file)
    pub old_string: String,
    /// Replacement text (must differ from old_string)
    pub new_string: String,
    /// Replace all occurrences (default: false)
    #[serde(default)]
    pub replace_all: bool,
}
```

### Validation Checklist

1. old_string != new_string
2. File exists (unless creating new via empty old_string)
3. File was previously read in this conversation
4. old_string found in file
5. old_string is unique OR replace_all=true
6. File not modified externally since last read

### Error Messages

```
"old_string and new_string must be different"
"You must use the Read tool to read the file before editing"
"Text not found in file: {preview}"
"Text appears {n} times. Use replace_all: true or provide more context"
"File was modified externally. Re-read before editing"
```

### Token Efficiency Tips for System Prompt

1. Use `replace_all: true` for renaming variables/functions across file
2. Include 2-3 lines of surrounding context for uniqueness
3. Preserve exact whitespace from Read output
4. Use line number prefixes from Read output carefully

---

## 8. Key Sources

- Claude Code analysis: github.com/shareAI-lab/analysis_claude_code
- OpenCode docs: opencode.ai/docs/tools/
- Codex apply_patch: platform.openai.com/docs/guides/tools-apply-patch
- Kiro analysis: github.com/ghuntley/amazon-kiro.kiro-agent-source-code-analysis
- Pi-Mono: github.com/badlogic/pi-mono
