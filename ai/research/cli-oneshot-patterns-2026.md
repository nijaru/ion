# AI CLI One-Shot Mode Patterns (January 2026)

Research on non-interactive/one-shot modes across major AI coding CLI tools.

## Summary Table

| Tool        | One-Shot Flag      | Stdin                   | Model Flag                        | Output Format                             | Auto-Approve            |
| ----------- | ------------------ | ----------------------- | --------------------------------- | ----------------------------------------- | ----------------------- |
| Claude Code | `-p` / `--print`   | `cat file \| claude -p` | `--model`                         | `--output-format text\|json\|stream-json` | `--permission-mode`     |
| Gemini CLI  | `-p` / `--prompt`  | Pipe supported          | `-m` / `--model`                  | `--output-format text\|json\|stream-json` | `-y` / `--yolo`         |
| OpenCode    | `run "prompt"`     | `-f file`               | `-m` / `--model` (provider/model) | `--format default\|json`                  | Config-based            |
| Codex CLI   | `exec "task"`      | `exec -`                | `-m` / `--model`                  | `--json` (JSONL), `-o` file               | `--full-auto`, `--yolo` |
| aider       | `-m` / `--message` | `-f` / `--message-file` | `--model`                         | `--stream`, `--pretty`                    | `--yes`                 |
| goose       | `run -t "text"`    | `run -i -`              | `--model`, `--provider`           | `--output-format text\|json\|stream-json` | Config-based            |

## Detailed Analysis

### 1. Claude Code CLI (Anthropic)

**One-shot syntax:**

```bash
claude -p "explain this function"
claude --print "fix the bug in main.rs"
```

**Stdin piping:**

```bash
cat error.log | claude -p "explain this error"
git diff | claude -p "review these changes"
```

**Model selection:**

```bash
claude --model sonnet "query"        # Alias
claude --model opus "query"          # Alias
claude --model claude-sonnet-4-5-20250929 "query"  # Full name
```

**Output formats:**

```bash
claude -p "query" --output-format text        # Default, human-readable
claude -p "query" --output-format json        # Final result as JSON
claude -p "query" --output-format stream-json # JSONL events as they occur
```

**Other notable flags:**

- `--max-turns 3` - Limit agentic turns
- `--max-budget-usd 5.00` - Cost limit
- `--verbose` - Full turn-by-turn output
- `--no-session-persistence` - Don't save session
- `--json-schema '{...}'` - Structured output validation

### 2. Gemini CLI (Google)

**One-shot syntax:**

```bash
gemini -p "generate a list of cat names"
gemini --prompt "explain closures in JavaScript"
```

**Stdin piping:**

```bash
cat code.py | gemini -p "review this code"
echo "explain X" | gemini -p -
```

**Model selection:**

```bash
gemini -p "query" -m gemini-2.5-flash
gemini -p "query" --model gemini-2.5-pro
```

**Output formats:**

```bash
gemini -p "query"                           # Text (default)
gemini -p "query" --output-format json      # Structured JSON with metadata
gemini -p "query" --output-format stream-json  # JSONL events
```

**Other notable flags:**

- `-y` / `--yolo` - Auto-approve all actions
- `--approval-mode auto_edit` - Approval policy
- `--debug` / `-d` - Debug mode
- `--include-directories src,docs` - Additional context

### 3. OpenCode (sst.dev/Anomaly)

**One-shot syntax:**

```bash
opencode run "explain how closures work in JavaScript"
opencode run "fix the type errors in src/"
```

**File attachment (no direct stdin):**

```bash
opencode run -f screenshot.png "explain this error"
opencode run --file code.ts "review this file"
```

**Model selection:**

```bash
opencode run -m anthropic/claude-sonnet-4 "query"
opencode run --model openai/gpt-4o "query"
```

**Output formats:**

```bash
opencode run "query"                  # Default formatted output
opencode run --format json "query"    # Raw JSON events
```

**Other notable flags:**

- `--attach http://localhost:4096` - Connect to running server
- `--share` - Share the session
- `-c` / `--continue` - Continue last session
- `--agent <name>` - Use specific agent

### 4. Codex CLI (OpenAI)

**One-shot syntax:**

```bash
codex exec "build a todo app"
codex e "fix the failing tests"    # Short alias
```

**Stdin piping:**

```bash
codex exec -                        # Read prompt from stdin
echo "explain this" | codex exec -
```

**Model selection:**

```bash
codex exec --model gpt-5-codex "task"
codex exec -m o1 "task"
codex exec --oss "task"             # Use local Ollama
```

**Output formats:**

```bash
codex exec "task"                           # Formatted text (stderr: progress, stdout: result)
codex exec --json "task"                    # JSONL stream to stdout
codex exec -o result.txt "task"             # Write final message to file
codex exec --output-schema schema.json "task"  # Validate against JSON schema
```

**Other notable flags:**

- `--full-auto` - Low-friction automation preset
- `--sandbox read-only|workspace-write|danger-full-access`
- `--yolo` / `--dangerously-bypass-approvals-and-sandbox`
- `--skip-git-repo-check` - Allow running outside git repo
- `codex exec resume [SESSION_ID]` - Resume previous session

### 5. aider

**One-shot syntax:**

```bash
aider --message "add docstrings to all functions" file.py
aider -m "fix the bug" src/*.py
```

**File-based input:**

```bash
aider --message-file instructions.txt file.py
aider -f tasks.md src/
```

**Model selection:**

```bash
aider --model claude-3-5-sonnet-20241022 file.py
aider --model gpt-4o file.py
# Deprecated shortcuts still work:
aider --sonnet file.py
aider --opus file.py
```

**Output control:**

```bash
aider -m "query" --no-stream        # Disable streaming
aider -m "query" --no-pretty        # Plain output
aider -m "query" --verbose          # Verbose logging
```

**Other notable flags:**

- `--yes` - Auto-confirm all prompts
- `--auto-commits` / `--no-auto-commits`
- `--test-cmd "pytest"` - Run tests after changes
- `--lint-cmd "ruff check"` - Run linter
- `--edit-format` - Control edit format (diff, whole, etc.)

### 6. goose (Block)

**One-shot syntax:**

```bash
goose run -t "explain this codebase"
goose run --text "fix the ESLint errors"
```

**Instruction file / stdin:**

```bash
goose run -i instructions.txt       # From file
goose run -i -                      # From stdin
goose run --instructions build.md
```

**Model selection:**

```bash
goose run --provider anthropic --model claude-sonnet-4 -t "query"
goose run --provider openai --model gpt-4o -t "query"
```

**Output formats:**

```bash
goose run -t "query"                          # Text (default)
goose run -t "query" --output-format json     # JSON after completion
goose run -t "query" --output-format stream-json  # JSONL as events occur
goose run -t "query" -q                       # Quiet mode (response only)
```

**Other notable flags:**

- `-s` / `--interactive` - Stay in interactive mode after
- `--no-session` - Don't persist session
- `-n` / `--name` - Name the session
- `--max-turns 10` - Limit turns
- `--with-builtin developer,git` - Enable extensions
- `--debug` - Detailed tool output
- `--recipe file.yaml` - Load recipe
- `--system "additional instructions"` - Extra system prompt

## Pattern Analysis

### Most Common Conventions

**One-shot flag patterns:**

1. **Subcommand style**: `tool run/exec "prompt"` (OpenCode, Codex, goose)
2. **Flag style**: `tool -p/-m "prompt"` (Claude, Gemini, aider)

**Stdin patterns:**

1. **Pipe to flag**: `cat file | tool -p "analyze"` (Claude, Gemini)
2. **Dash convention**: `tool exec -` or `tool run -i -` (Codex, goose)
3. **File flag**: `tool -f file` (aider, OpenCode)

**Model selection:**

- Universal: `--model` / `-m` flag
- Format varies: `model-name` vs `provider/model`

**Output format:**

- Emerging standard: `--output-format text|json|stream-json`
- Used by: Claude, Gemini, goose
- Codex uses: `--json` boolean flag

### Recommendations for ion

Based on industry patterns, recommend:

```bash
# Primary one-shot (subcommand style - more explicit)
ion run "prompt"
ion run -t "prompt"           # Explicit text flag

# Alternative flag style (familiar to Claude users)
ion -p "prompt"

# Stdin
ion run -                     # Read from stdin
cat file | ion run -          # Pipe content
ion run -f file.txt           # File input

# Model selection
ion --model deepseek/deepseek-v4 run "prompt"
ion -m anthropic/claude-sonnet-4 run "prompt"

# Output format (follow emerging standard)
ion run --output-format text "prompt"       # Default
ion run --output-format json "prompt"       # Final JSON
ion run --output-format stream-json "prompt" # JSONL events
ion run -q "prompt"                         # Quiet (response only)

# Other useful flags
ion run --max-turns 5 "prompt"
ion run --no-session "prompt"
ion run --verbose "prompt"
```

**Key conventions to follow:**

1. `-p` / `--print` for Claude Code compatibility
2. `run` subcommand for explicit non-interactive mode
3. `-` for stdin (POSIX convention)
4. `--output-format` with `text|json|stream-json` options
5. `-q` / `--quiet` for minimal output
6. `--model` in `provider/model` format

## Sources

- Claude Code CLI: https://docs.anthropic.com/en/docs/claude-code/cli-usage
- Gemini CLI: https://geminicli.com/docs/cli/headless/
- OpenCode: https://opencode.ai/docs/cli/
- Codex CLI: https://developers.openai.com/codex/cli/reference/
- aider: https://aider.chat/docs/scripting.html
- goose: https://block.github.io/goose/docs/guides/goose-cli-commands/

---

_Research date: 2026-01-18_
