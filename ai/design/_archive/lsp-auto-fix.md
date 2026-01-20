# LSP Auto-Fix Integration

**Created**: 2025-11-20
**Status**: Design (Ready for Implementation)
**Estimated Effort**: 4-6 hours
**Expected Impact**: 20-30% token savings on code tasks

## Problem

Agents waste tokens on preventable errors:
- **Syntax errors**: LLM generates invalid code → test → detect → fix → retest (2-3 iterations wasted)
- **Import issues**: Missing imports → runtime error → add import → retest
- **Formatting**: Inconsistent style → manual fixing
- **Type errors**: Gradual typing issues caught late

**Example Token Waste** (count-dataset-tokens task):
```
WITHOUT LSP:
Iteration 1: Write code with syntax error
Iteration 2: Run code, see SyntaxError
Iteration 3: Fix syntax, see ImportError
Iteration 4: Add import, code works
Total: 4 iterations, ~12K tokens

WITH LSP:
Iteration 1: Write code → LSP catches syntax + missing import → auto-fix
Iteration 2: Code works
Total: 2 iterations, ~6K tokens (50% savings)
```

## Solution: LSP Auto-Fix Hook

**Integration Point**: Terminal-Bench adapter's code execution workflow

**Hook Pattern**:
```python
# terminal_bench_adapter/aircher_agent.py

def execute_code_action(self, command: str) -> dict:
    """Execute code with LSP pre-validation"""

    # Detect code write operations
    if self._is_code_write(command):
        file_path = self._extract_file_path(command)
        new_content = self._extract_content(command)

        # PRE-EXECUTION LSP CHECK
        lsp_result = self.lsp_autofix.validate_and_fix(
            file_path, new_content
        )

        if lsp_result["has_errors"]:
            # Show errors to LLM WITHOUT executing
            return {
                "success": False,
                "error_type": "lsp_validation_failed",
                "diagnostics": lsp_result["errors"],
                "suggestion": "Fix LSP errors before proceeding"
            }

        # Auto-apply fixes
        if lsp_result["auto_fixes_available"]:
            command = self._update_command_with_fixes(
                command, lsp_result["fixed_content"]
            )

    # Execute (with or without LSP fixes)
    return self._execute_bash(command)
```

## Terminal-Bench Integration (Implementation Guide)

### Step 1: Initialize LSP in `__init__` (line 450-505)

```python
# After line 504 (after vLLM client initialization):

# Initialize LSP auto-fix (optional, enabled via env var)
self.lsp_enabled = os.getenv("AIRCHER_LSP_ENABLED", "false").lower() == "true"
self.lsp_autofix: PythonLSPAutoFix | None = None
if self.lsp_enabled:
    try:
        self.lsp_autofix = PythonLSPAutoFix()
        self._logger.info("LSP auto-fix enabled")
    except Exception as e:
        self._logger.warning(f"LSP auto-fix disabled: {e}")
        self.lsp_enabled = False
```

### Step 2: Add Helper Methods (after line 600)

```python
def _detect_python_write(self, command: str) -> tuple[bool, str | None, str | None]:
    """Detect Python code write commands.

    Returns:
        (is_python_write, file_path, content)
    """
    # Pattern 1: cat > file.py << 'EOF'
    heredoc_match = re.search(r"cat\s+>\s+([^\s<]+\.py)\s+<<\s*['\"]?EOF['\"]?", command)
    if heredoc_match:
        file_path = heredoc_match.group(1)
        # Extract content between << 'EOF' and EOF
        content_match = re.search(
            r"<<\s*['\"]?EOF['\"]?\n(.*?)\nEOF",
            command,
            re.DOTALL
        )
        if content_match:
            return (True, file_path, content_match.group(1))

    # Pattern 2: echo 'code' > file.py
    echo_match = re.search(r"echo\s+['\"](.+?)['\"]\s+>\s+([^\s]+\.py)", command)
    if echo_match:
        return (True, echo_match.group(2), echo_match.group(1))

    # Pattern 3: python -c "code"
    if "python -c" in command or "python3 -c" in command:
        code_match = re.search(r"python[3]?\s+-c\s+['\"](.+?)['\"]", command)
        if code_match:
            return (True, "/tmp/inline.py", code_match.group(1))

    return (False, None, None)
```

### Step 3: Add Validation Hook (before line 1208)

```python
# Before: session.send_keys([action.command, "Enter"], block=False)
# Add LSP validation:

# LSP PRE-EXECUTION VALIDATION
if self.lsp_enabled and self.lsp_autofix:
    is_python_write, file_path, content = self._detect_python_write(action.command)

    if is_python_write and content:
        self._logger.info(f"LSP validating: {file_path}")
        lsp_result = self.lsp_autofix.validate_and_fix(file_path, content)

        if lsp_result["has_errors"]:
            # Show errors to agent WITHOUT executing
            error_summary = "\n".join(
                f"  Line {e['line']}: {e['msg']}" for e in lsp_result["errors"][:5]
            )
            observation = f"LSP validation failed for {file_path}:\n{error_summary}"

            context.add_interaction(
                thought=action.thought,
                command=action.command,
                observation=observation,
                success=False
            )

            self._logger.warning(f"LSP blocked execution due to {len(lsp_result['errors'])} errors")
            continue  # Skip execution, let agent fix errors

        elif lsp_result["auto_fixes_available"]:
            # Apply auto-fixes to command
            fixed_content = lsp_result["fixed_content"]
            # Replace content in command with fixed version
            action.command = self._update_command_content(
                action.command, fixed_content
            )
            self._logger.info(f"LSP applied auto-fixes to {file_path}")

# Then execute (possibly LSP-fixed) command
session.send_keys([action.command, "Enter"], block=False)
```

### Step 4: Add Content Replacement Helper

```python
def _update_command_content(self, command: str, fixed_content: str) -> str:
    """Replace code content in command with LSP-fixed version."""
    # For heredoc: replace content between << 'EOF' and EOF
    if "<<" in command and "EOF" in command:
        return re.sub(
            r"(<<\s*['\"]?EOF['\"]?\n).*?(\nEOF)",
            r"\1" + fixed_content + r"\2",
            command,
            flags=re.DOTALL
        )

    # For echo: replace quoted content
    if "echo" in command:
        return re.sub(
            r"(echo\s+['\"])(.+?)(['\"])",
            r"\1" + fixed_content + r"\3",
            command
        )

    return command  # Fallback: no replacement
```

## Implementation Phases

### Phase 1: Python LSP Integration (Quick Win)

**Library**: `python-lsp-server` (pylsp)
**Setup**: ~30 min
**Impact**: 20-30% token savings on Python tasks

```bash
uv add python-lsp-server python-lsp-ruff
```

```python
# src/aircher/tools/lsp_autofix.py

import subprocess
import json
from pathlib import Path

class PythonLSPAutoFix:
    """LSP auto-fix for Python code"""

    def __init__(self):
        self.pylsp_path = "pylsp"  # From uv-installed bin

    def validate_and_fix(self, file_path: str, content: str) -> dict:
        """
        Validate Python code with LSP and auto-fix if possible

        Returns:
            {
                "has_errors": bool,
                "errors": list[dict],  # {line, col, msg, severity}
                "auto_fixes_available": bool,
                "fixed_content": str | None
            }
        """

        # 1. Write temp file
        temp_file = Path("/tmp/aircher_lsp") / Path(file_path).name
        temp_file.parent.mkdir(parents=True, exist_ok=True)
        temp_file.write_text(content)

        # 2. Get diagnostics (syntax + type errors)
        diagnostics = self._get_diagnostics(temp_file)

        # 3. Auto-format with ruff
        formatted_content = self._format_with_ruff(temp_file)

        # 4. Apply auto-fixes (organize imports, etc.)
        fixed_content = self._apply_code_actions(temp_file)

        # 5. Recheck after fixes
        final_diagnostics = self._get_diagnostics(temp_file)

        return {
            "has_errors": any(d["severity"] == "error" for d in final_diagnostics),
            "errors": [d for d in final_diagnostics if d["severity"] == "error"],
            "auto_fixes_available": len(final_diagnostics) < len(diagnostics),
            "fixed_content": fixed_content if fixed_content != content else None
        }

    def _get_diagnostics(self, file_path: Path) -> list[dict]:
        """Get LSP diagnostics via pylsp"""

        # Use pylsp via subprocess (simpler than LSP protocol)
        result = subprocess.run(
            ["ruff", "check", "--output-format=json", str(file_path)],
            capture_output=True,
            text=True
        )

        diagnostics = json.loads(result.stdout) if result.stdout else []

        return [
            {
                "line": d["location"]["row"],
                "col": d["location"]["column"],
                "msg": d["message"],
                "severity": "error" if d["code"].startswith("E") else "warning",
                "code": d["code"]
            }
            for d in diagnostics
        ]

    def _format_with_ruff(self, file_path: Path) -> str:
        """Auto-format with ruff"""

        subprocess.run(
            ["ruff", "format", str(file_path)],
            capture_output=True
        )

        return file_path.read_text()

    def _apply_code_actions(self, file_path: Path) -> str:
        """Apply auto-fixable issues"""

        subprocess.run(
            ["ruff", "check", "--fix", str(file_path)],
            capture_output=True
        )

        return file_path.read_text()
```

**Integration**:
```python
# terminal_bench_adapter/aircher_agent.py

from aircher.tools.lsp_autofix import PythonLSPAutoFix

class AircherTerminalBenchAdapter:
    def __init__(self, ...):
        ...
        self.lsp_autofix = PythonLSPAutoFix()

    def _is_code_write(self, command: str) -> bool:
        """Detect Python file writes"""
        return any(
            pattern in command
            for pattern in [
                "cat >", "cat <<", "echo >",
                "python -c", "cat > *.py",
                "tee *.py"
            ]
        )

    def _extract_file_path(self, command: str) -> str | None:
        """Extract target file path from command"""

        import re

        # cat > file.py
        if match := re.search(r"cat\s+>\s+(\S+\.py)", command):
            return match.group(1)

        # python -c "..."
        if "python -c" in command:
            return "/tmp/inline_script.py"

        return None

    def perform_task(self, instruction: str) -> dict:
        """Override with LSP validation"""

        # ... existing agent loop ...

        # Before executing action.command:
        if self._is_code_write(action.command):
            file_path = self._extract_file_path(action.command)

            if file_path and file_path.endswith(".py"):
                # Extract content from command
                content = self._extract_content(action.command)

                # LSP validate + auto-fix
                lsp_result = self.lsp_autofix.validate_and_fix(
                    file_path, content
                )

                if lsp_result["has_errors"]:
                    # Show errors to agent
                    observation = (
                        f"LSP validation failed:\n"
                        + "\n".join(
                            f"  Line {e['line']}: {e['msg']}"
                            for e in lsp_result["errors"]
                        )
                    )

                    # Add to history, DON'T execute command
                    history.append({
                        "action": action.command,
                        "observation": observation
                    })
                    continue  # Next iteration

                # Apply auto-fixes if available
                if lsp_result["fixed_content"]:
                    action.command = self._update_command_with_content(
                        action.command, lsp_result["fixed_content"]
                    )

        # Execute (possibly LSP-fixed) command
        result = session.send_keys([action.command, "Enter"])
        ...
```

### Phase 2: Multi-Language Support (Future)

**Languages**: JavaScript, TypeScript, Rust, Go
**LSP Servers**: `typescript-language-server`, `rust-analyzer`, `gopls`
**Effort**: +2h per language
**When**: After Phase 1 validates 20-30% token savings

## Expected Impact

**Baseline** (v15, 10 tasks, no LSP):
- Estimated accuracy: 30-40%
- Avg iterations per task: 25-50
- Token usage: ~60K input, ~3K output per task

**With LSP Auto-Fix** (Phase 1, Python only):
- Estimated accuracy: 40-50% (+10pp)
- Avg iterations per task: 15-35 (-30%)
- Token usage: ~40K input, ~2K output per task (-30%)

**Saved Tokens** (10 Python tasks):
- Without LSP: 10 × 60K = 600K tokens
- With LSP: 10 × 40K = 400K tokens
- **Savings: 200K tokens (33%)**

At $0.26/$1.02 per M (minimax-m2 rates):
- Input savings: 200K × $0.26/M = **$0.05 per run**
- Output savings: 10K × $1.02/M = **$0.01 per run**
- Total: **$0.06 per 10-task run**

More importantly: **Faster completion** (iterations matter more than cost with Grok-4.1-fast free tier)

## Testing Plan

1. **Unit tests**: LSP diagnostics, formatting, auto-fix application
2. **Integration tests**: Terminal-Bench adapter LSP hook
3. **Validation**: Re-run v15 baseline with LSP enabled
   - Tasks: hello-world, broken-python, simple-web-scraper (Python-heavy)
   - Metrics: Accuracy delta, iteration delta, token delta
   - Success: +10pp accuracy OR -20% iterations

## Rollout

1. **Implement Phase 1** (4-6h): Python LSP integration
2. **Test with 3 Python tasks** (1h): Validate token savings
3. **Run full v16 benchmark** (2h): LSP-enabled 10-task baseline
4. **Analyze results** (1h): Compare v15 vs v16
5. **Decide on Phase 2**: If 20%+ token savings confirmed, add JS/TS/Rust

## Non-Goals (Explicitly NOT Doing)

- ❌ Full LSP protocol client (use ruff CLI instead, simpler)
- ❌ Language server management (assume pylsp/ruff installed)
- ❌ IDE-like features (hover, autocomplete) - only validation + auto-fix
- ❌ Cross-language analysis (focus on single-file validation)

## Open Questions

1. **How to handle heredocs?** Extract content before LSP validation?
   - **Answer**: Parse heredoc content, validate, update heredoc body
2. **What if LSP not installed?** Graceful degradation?
   - **Answer**: Check for `ruff` binary, disable LSP if missing (log warning)
3. **Performance impact?** LSP adds ~100-200ms per code write
   - **Answer**: Acceptable - LLM calls take 5-10s anyway

## References

- **Research**: `ai/research/codebase-intelligence-options.md` (lines 152-253)
- **LSP Spec**: https://microsoft.github.io/language-server-protocol/
- **Ruff Docs**: https://docs.astral.sh/ruff/
- **Similar work**: Claude Code uses LSP for diagnostics (confirmed in research)
