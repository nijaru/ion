# v14 Improvements: SOTA Patterns Implementation

**Research Date**: 2025-11-20
**Based On**: Claude Code, OpenAI Codex, SWE-Agent, OpenCode analysis
**Expected Impact**: +8-13pp (38-43% accuracy)
**Total Effort**: ~20 hours

---

## Context

**Current State** (v13):
- ✅ Multi-turn completion detection
- ✅ Auto-compaction at 80% token limit
- ✅ Task complexity classification (3-tier)
- ✅ Output limits (prompt-only, not enforced)
- ✅ Command chaining guidance
- ✅ Format validation guidance

**Gap Analysis**:
We've implemented the "soft" patterns (prompts, loop structure) but missing "hard" patterns (tool enforcement, error recovery, real-time feedback).

---

## 1. Enforce Output Limits in Tools

**Priority**: HIGH | **Effort**: 4-6h | **Impact**: +2-3pp

### Current Problem
```python
# terminal_bench_adapter/aircher_agent.py
# Prompt says "limit to ~100 lines" but tools don't enforce
output = execute_command(cmd)  # Could be 5000 lines
```

### SOTA Pattern (SWE-Agent)
```python
class BashTool:
    MAX_OUTPUT_LINES = 100

    def execute(self, command: str) -> dict:
        output = run_command(command)
        lines = output.split('\n')

        if len(lines) > self.MAX_OUTPUT_LINES:
            return {
                'stdout': '\n'.join(lines[:self.MAX_OUTPUT_LINES]),
                'metadata': {
                    'lines_shown': self.MAX_OUTPUT_LINES,
                    'lines_hidden': len(lines) - self.MAX_OUTPUT_LINES,
                    'truncated': True
                },
                'message': f"[Output truncated: showing first {self.MAX_OUTPUT_LINES}/{len(lines)} lines]"
            }

        return {'stdout': output, 'truncated': False}
```

### Why It Works
- **Hard limit**: Model can't accidentally waste context
- **Metadata**: Model knows how much was hidden
- **Explicit**: Clear feedback when truncation happens

### Implementation Location
- File: `terminal_bench_adapter/aircher_agent.py`
- Method: `_execute_bash_command()` around line 1000-1100
- Add truncation before returning result to model

---

## 2. Verification Command Pattern

**Priority**: HIGH | **Effort**: 2-3h | **Impact**: +2-3pp

### Current Problem
Agent often forgets to verify work before marking complete.

### SOTA Pattern (Terminal-Bench analysis + Codex)
```python
VERIFICATION_PATTERN = """
VERIFICATION COMMANDS (use BEFORE marking complete):

File creation tasks:
- "test -f /path/to/file && wc -l /path/to/file"  # Confirm exists + size
- "ls -lh /path/to/file"  # Check permissions, size

Script execution tasks:
- "./script.sh && echo 'SUCCESS' || echo 'FAILED'"  # Confirm exit code
- "python script.py 2>&1 | tail -20"  # Check for errors

Format validation tasks:
- "head -5 output.txt"  # Verify headers/structure
- "tail -5 output.txt"  # Verify end/completeness
- "wc -l output.txt"  # Confirm line count matches expected

Test suite tasks:
- Run FULL test suite, check ALL tests pass (not just sample)
- "pytest -v | grep -E 'PASSED|FAILED' | tail -10"  # Summary

Build tasks:
- "make clean && make && ./binary --version"  # Full rebuild + verification

NEVER mark complete without explicit verification commands showing success.
"""
```

### Implementation Location
- File: `terminal_bench_adapter/aircher_agent.py`
- Section: Add to system prompt after "COMPLETION CRITERIA" (line ~370)
- Update: Add examples for common task types

---

## 3. Real-Time Token Display

**Priority**: MEDIUM | **Effort**: 2-3h | **Impact**: +1-2pp

### Current Problem
Model doesn't know when context is filling up, writes verbosely.

### SOTA Pattern (OpenAI Codex)
```python
def build_observation(self, output: str, tokens_used: int, context_window: int) -> str:
    percent = (tokens_used / context_window) * 100

    observation = f"""
[Token usage: {tokens_used:,}/{context_window:,} ({percent:.0f}%)]

{output}
"""

    # Warning at 60%, 80%
    if percent >= 80:
        observation += "\n⚠️ WARNING: Context at 80%+ - be very concise in responses"
    elif percent >= 60:
        observation += "\n⚠️ Context at 60%+ - aim for concise responses"

    return observation
```

### Why It Works
- **Awareness**: Model knows when to be concise
- **Early warning**: Can adjust before hitting limit
- **Concrete numbers**: "45,000/128,000 (35%)" more actionable than silent

### Implementation Location
- File: `terminal_bench_adapter/aircher_agent.py`
- Class: `IterationContext`
- Method: `add_observation()` around line 200-250
- Add token display prefix to all observations

---

## 4. Structured Error Recovery

**Priority**: HIGH | **Effort**: 8-10h | **Impact**: +3-5pp

### Current Problem
```python
# When command fails:
if exit_code != 0:
    observation = f"Command failed: {stderr}"
    # Model sees error but no guidance on recovery
```

### SOTA Pattern (OpenAI Codex)
```python
class FailureAnalyzer:
    def analyze(self, command: str, exit_code: int, stderr: str) -> dict:
        """Categorize failure and suggest recovery"""

        # Pattern matching on common errors
        if "permission denied" in stderr.lower():
            return {
                'category': 'PermissionDenied',
                'recoverable': True,
                'escalation_needed': True,
                'suggestion': 'Try with sudo or change file permissions'
            }

        elif "no such file" in stderr.lower():
            return {
                'category': 'NotFound',
                'recoverable': True,
                'escalation_needed': False,
                'suggestion': 'Check path, try "find" or "ls" to locate file'
            }

        elif "command not found" in stderr.lower():
            return {
                'category': 'CommandNotFound',
                'recoverable': True,
                'escalation_needed': False,
                'suggestion': 'Install package or check PATH'
            }

        # ... more patterns

        return {
            'category': 'Unknown',
            'recoverable': False,
            'suggestion': 'Read error message carefully and try alternative approach'
        }

class ErrorRecovery:
    def __init__(self):
        self.analyzer = FailureAnalyzer()
        self.failure_counts = {}  # Track repeated failures

    def handle_failure(self, command: str, exit_code: int, stderr: str) -> str:
        """Build recovery-focused observation"""

        analysis = self.analyzer.analyze(command, exit_code, stderr)

        # Track repeated failures
        key = (command, analysis['category'])
        self.failure_counts[key] = self.failure_counts.get(key, 0) + 1

        observation = f"""
Command failed with exit code {exit_code}

Error category: {analysis['category']}
Recoverable: {analysis['recoverable']}

Stderr:
{stderr}

Suggested recovery:
{analysis['suggestion']}
"""

        # Escalation warning
        if self.failure_counts[key] >= 3:
            observation += f"""
⚠️ WARNING: This command has failed {self.failure_counts[key]} times.
Consider a completely different approach.
"""

        # Escalation offer
        if analysis['escalation_needed']:
            observation += f"""
💡 ESCALATION AVAILABLE: Try riskier approach if safer method keeps failing.
Example: sudo, chmod 777, --force flags, etc.
"""

        return observation
```

### Why It Works
- **Categorization**: Model understands *why* it failed
- **Suggestions**: Concrete next steps instead of blind retry
- **Escalation**: Break deadlocks by allowing riskier approaches
- **Tracking**: Detect infinite loops early

### Implementation Location
- File: `terminal_bench_adapter/aircher_agent.py`
- New class: `FailureAnalyzer` (add after imports)
- New class: `ErrorRecovery` (add after FailureAnalyzer)
- Modify: `_execute_bash_command()` to use ErrorRecovery on failures

---

## Implementation Order

### Week 1 (Quick Wins)
1. **Day 1-2**: Verification command pattern (2-3h)
   - Add to prompt
   - Test on 3 quick tasks

2. **Day 2-3**: Real-time token display (2-3h)
   - Modify IterationContext
   - Test with long task

3. **Day 3-4**: Enforce output limits (4-6h)
   - Modify _execute_bash_command()
   - Add truncation logic
   - Test with verbose commands

### Week 2 (Major Feature)
4. **Day 5-7**: Structured error recovery (8-10h)
   - Build FailureAnalyzer class
   - Build ErrorRecovery class
   - Integrate with command execution
   - Test on failing tasks

---

## Expected Results

**Baseline** (v13): TBD (running now)

**After v14 Quick Wins** (items 1-3):
- Expected: +4-6pp
- Validation: Run 3-task quick test
- Time: ~3-4 days

**After v14 Complete** (all 4 items):
- Expected: +8-13pp total
- Target: 38-43% accuracy (beating Claude Code 43.2% baseline)
- Validation: Full 10-task benchmark
- Time: ~7-10 days

---

## Success Criteria

### v14 Quick Test (3 tasks)
- ✅ No output truncation failures (model knows when truncated)
- ✅ All tasks run verification before completion
- ✅ Token warnings appear in logs

### v14 Full Benchmark (10 tasks)
- ✅ Accuracy >38% (minimum success)
- ✅ Accuracy >43% (beat Claude Code - stretch goal)
- ✅ Zero tasks fail from context overflow
- ✅ Error recovery attempts visible in logs

---

## Deferred Patterns (Post-v14)

These patterns have high effort or require v14 results first:

### Sub-Agent Verification (v15)
- **Effort**: 12-16h
- **Impact**: +4pp
- **Reason to defer**: Want to see v14 results first, higher effort

### Task-Specific Prompts (v16)
- **Effort**: 16-20h
- **Impact**: +3pp
- **Reason to defer**: Need baseline with current improvements first

### Windowed File Viewer (v16)
- **Effort**: 6-8h
- **Impact**: +2pp
- **Reason to defer**: Terminal-Bench doesn't emphasize file reading

---

## References

- **Codex CLI Architecture**: `ai/research/codex-cli-architecture.md`
- **SWE-Agent Scaffolding**: `ai/research/agent-scaffolding.md`
- **TUI Agents SOTA**: `ai/research/tui-agents-sota-2025.md`
- **Codex Roadmap**: `ai/research/codex-improvement-roadmap.md`

---

## Monitoring

Track these metrics during v14 development:

1. **Context Efficiency**: Tokens used per iteration (should decrease)
2. **Verification Rate**: % of tasks that verify before completion (should be 100%)
3. **Error Recovery**: % of failures that trigger suggestions (should be >80%)
4. **Truncation Events**: Count of output truncations (should match verbose commands)

---

## Notes

**Why these 4 patterns?**
- All proven in SOTA agents (Codex 57.8%, SWE-Agent 3-5x improvement)
- Complementary (each addresses different failure mode)
- Incremental (can implement independently)
- Measurable (clear before/after metrics)

**Why not sub-agent verification first?**
- Higher effort (12-16h vs 2-8h per pattern)
- Want to validate quick wins first
- May not need +4pp if v14 quick wins get us to 43%+
