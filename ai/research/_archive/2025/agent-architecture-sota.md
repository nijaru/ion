# Agent Architecture: SOTA Comparison & Fix Plan

**Created**: 2025-11-16
**Purpose**: Compare current agent implementation against SOTA patterns, identify gaps, prioritize fixes

## Executive Summary

Current implementation hits **20% accuracy ceiling** on Terminal-Bench. Root cause: Missing critical SOTA scaffolding patterns that provide **3-5x improvement** per SWE-Agent research.

**Priority fixes** (highest ROI):
1. **Adaptive planning** - Revise plan when errors occur
2. **Context window management** - Compress at 80%, smart windowing
3. **Error recovery loop** - Retry with different approach on failure
4. **Search/output limits** - Max 50 results, 100 lines

## Current Implementation Analysis

### `terminal_bench_adapter/aircher_agent.py`

| Component | Current | SOTA Pattern | Gap Severity |
|-----------|---------|--------------|--------------|
| **Planning** | Static plan (L324-406) | Adaptive plan-before-code | CRITICAL |
| **Terminal Output** | 50 lines (L201) | 100-line windowed viewer | HIGH |
| **Memory Usage** | Write-only (L607-620) | Query for similar tasks | HIGH |
| **Context Management** | None | Last 5 interactions + compress at 92% | CRITICAL |
| **Error Recovery** | JSON parsing only (L212-309) | Strategic retry with alternative | HIGH |
| **Plan Adaptation** | Never adapts | Revise on error/failure | CRITICAL |
| **Verification** | Pattern matching (L533-559) | Semantic verification | MEDIUM |
| **Search Limits** | None | Max 50 results | HIGH |

## SOTA Patterns from Research

### 1. Adaptive Planning (Claude Code Pattern)

**Current** (L386-406):
```python
plan = self._create_plan(task, llm)  # Once, never changes
# ... iterations use same plan
```

**SOTA Pattern**:
```python
plan = self._create_plan(task, llm)
for iteration in range(max_iterations):
    action = self._get_action(plan, context)
    if action.failed or action.stuck:
        # REVISE PLAN based on what we learned
        plan = self._revise_plan(task, plan, errors_encountered, context)
```

**Fix**: Add `_revise_plan()` method that updates plan when:
- Error occurs 2+ times
- Agent is stuck (5+ read-only commands)
- Approach clearly not working

### 2. Context Window Management (Claude Code at 92%)

**Current**: No management, flat message history, no compression

**SOTA Pattern**:
```python
class ContextManager:
    def add_interaction(self, thought, action, observation):
        self.history.append(...)
        if self.token_count() > 0.92 * self.max_tokens:
            self._compress_older_interactions()

    def _compress_older_interactions(self):
        # Keep last 5 full, summarize older
        recent = self.history[-5:]
        older = self.history[:-5]
        summary = self._summarize(older)
        self.history = [summary] + recent
```

**Fix**: Add `ContextManager` that:
- Tracks all interactions
- Compresses at 92% capacity
- Keeps last 5 full, summarizes older

### 3. Error Recovery Loop (SWE-Agent Pattern)

**Current**: Only handles JSON parsing errors, no strategic recovery

**SOTA Pattern**:
```python
for attempt in range(3):  # Max 3 attempts per step
    action = self._get_action(context)
    result = self._execute(action)

    if result.success:
        break
    elif result.error_type == "syntax_error":
        # Auto-rejected by linting, try different approach
        context.add_learning(f"Syntax error: {result.error}")
    elif result.error_type == "command_failed":
        # Try alternative command
        context.add_constraint(f"Avoid: {action.command}")
    else:
        # Unknown error, revise plan
        plan = self._revise_plan(...)
```

**Fix**: Wrap command execution in retry loop with:
- Track what failed and why
- Provide failed attempts as context to LLM
- Allow plan revision on repeated failures

### 4. Windowed File Viewer (SWE-Agent Critical)

**Current** (L201):
```python
def _get_terminal_output(self, session, lines=50):
    # Returns last 50 lines, no scrolling, no navigation
```

**SOTA Pattern**:
```python
def _get_terminal_output(self, session, lines=100):
    output = session.capture_pane(capture_entire=True)
    all_lines = output.strip().split("\n")

    # Return 100 lines with position info
    return {
        "content": "\n".join(all_lines[-lines:]),
        "total_lines": len(all_lines),
        "hidden_above": max(0, len(all_lines) - lines),
        "hint": f"Showing last {lines} of {len(all_lines)} lines"
    }
```

**Fix**: Increase to 100 lines, provide context about what's hidden

### 5. Search Result Limits (Critical for Cognitive Load)

**Current**: No limits on output

**SOTA Pattern**:
```python
MAX_SEARCH_RESULTS = 50

def _execute_search(self, command):
    result = subprocess.run(command, capture_output=True)
    lines = result.stdout.split('\n')

    if len(lines) > MAX_SEARCH_RESULTS:
        return {
            "results": lines[:MAX_SEARCH_RESULTS],
            "truncated": True,
            "total": len(lines),
            "hint": f"Showing first {MAX_SEARCH_RESULTS} of {len(lines)} results. Refine query to see more."
        }
    return {"results": lines, "truncated": False}
```

**Fix**: Detect search commands (grep, find, ls) and limit output

### 6. Memory Querying (Aircher's Unique Advantage - UNUSED)

**Current**: Write-only memory (L607-620)
```python
# Only records to memory, never queries it
self.memory.episodic.record_tool_execution(...)
```

**SOTA Pattern** (unique to Aircher):
```python
# Before starting task
if self.memory:
    similar_tasks = self.memory.episodic.get_similar_tasks(task)
    if similar_tasks:
        context.add(f"Similar tasks solved: {similar_tasks}")
        for task in similar_tasks:
            context.add(f"Approach that worked: {task.successful_approach}")

# During iteration
if action.failed:
    similar_failures = self.memory.episodic.get_similar_failures(action)
    context.add(f"Previously failed similar approach: {similar_failures}")
```

**Fix**: Query memory at:
- Task start: Similar successful tasks
- On error: Similar failures to avoid
- On completion: Record what worked

## Priority Fix Order (ROI-Based)

### Phase 1: Critical (Expected +50% accuracy gain)

1. **Adaptive Planning** (Lines 324-406)
   - Add `_revise_plan()` when stuck or errors
   - Effort: 2 hours
   - Impact: HIGH - prevents repeating failed approaches

2. **Context Window Management**
   - Add conversation history tracking
   - Compress at 92% usage
   - Keep last 5 interactions full
   - Effort: 3 hours
   - Impact: HIGH - prevents context overflow

3. **Error Recovery Loop** (Lines 527-630)
   - Track failed attempts
   - Provide failed context to LLM
   - Auto-retry with constraints
   - Effort: 2 hours
   - Impact: HIGH - learns from mistakes

### Phase 2: High Priority (Expected +25% accuracy gain)

4. **Increase Terminal Output** (Line 201)
   - Change 50 → 100 lines
   - Add position metadata
   - Effort: 30 minutes
   - Impact: MEDIUM - more context for complex tasks

5. **Search Result Limits**
   - Detect search commands
   - Limit to 50 results
   - Effort: 1 hour
   - Impact: MEDIUM - reduces cognitive load

6. **Memory Querying**
   - Query similar tasks at start
   - Query similar failures on error
   - Effort: 2 hours
   - Impact: MEDIUM - leverages our unique advantage

### Phase 3: Nice to Have

7. **Semantic Verification** (Lines 533-559)
   - Parse output for semantic success/failure
   - Effort: 3 hours
   - Impact: LOW - current heuristics work for most cases

## Implementation Plan

### Fix 1: Adaptive Planning

```python
def _revise_plan(self, task: str, current_plan: TaskPlan,
                 failures: list[str], iteration: int) -> TaskPlan:
    """Revise plan based on what we learned from failures."""

    revision_prompt = f"""The current plan is not working. Revise it.

ORIGINAL TASK: {task}

CURRENT PLAN:
{current_plan.approach}

STEPS ATTEMPTED: {current_plan.steps[:iteration]}

FAILURES ENCOUNTERED:
{chr(10).join(failures)}

Create a NEW plan that avoids the failed approaches. Respond with JSON:
{{
  "understanding": "...",
  "approach": "NEW approach that avoids failures",
  "steps": ["new step 1", ...],
  "success_criteria": "..."
}}"""

    return self._parse_plan(self._call_llm(revision_prompt))
```

### Fix 2: Context Manager

```python
class IterationContext:
    def __init__(self, max_tokens: int = 128000):
        self.max_tokens = max_tokens
        self.interactions: list[dict] = []
        self.failures: list[str] = []

    def add_interaction(self, thought: str, command: str,
                        observation: str, success: bool):
        self.interactions.append({
            "thought": thought,
            "command": command,
            "observation": observation,
            "success": success
        })
        self._compress_if_needed()

    def add_failure(self, failure: str):
        self.failures.append(failure)

    def _compress_if_needed(self):
        if self._estimate_tokens() > 0.92 * self.max_tokens:
            # Keep last 5 full, summarize older
            if len(self.interactions) > 5:
                older = self.interactions[:-5]
                summary = self._summarize_interactions(older)
                self.interactions = [{"summary": summary}] + self.interactions[-5:]

    def format_for_prompt(self) -> str:
        result = ""
        if self.failures:
            result += f"FAILED APPROACHES (avoid these):\n"
            for f in self.failures[-3:]:  # Last 3 failures
                result += f"- {f}\n"

        if self.interactions:
            result += "\nRECENT INTERACTIONS:\n"
            for i in self.interactions[-5:]:
                if "summary" in i:
                    result += f"[Earlier work summary: {i['summary']}]\n"
                else:
                    result += f"Thought: {i['thought']}\n"
                    result += f"Command: {i['command']}\n"
                    result += f"Result: {'Success' if i['success'] else 'Failed'}\n\n"
        return result
```

### Fix 3: Error Recovery

```python
# In perform_task loop:
consecutive_failures = 0
max_consecutive_failures = 3

for iteration in range(1, self.max_iterations + 1):
    action = self._get_action(context, plan)

    if action.command:
        success = self._execute_command(action.command, session)
        context.add_interaction(action.thought, action.command,
                                terminal_output, success)

        if not success:
            consecutive_failures += 1
            context.add_failure(f"{action.command} failed: {terminal_output}")

            if consecutive_failures >= max_consecutive_failures:
                # Revise plan - current approach not working
                plan = self._revise_plan(task, plan, context.failures, iteration)
                consecutive_failures = 0
        else:
            consecutive_failures = 0
```

## Expected Impact

Based on SWE-Agent research (3-5x improvement from scaffolding):

| Current | After Phase 1 | After Phase 2 | After Phase 3 |
|---------|--------------|--------------|--------------|
| 20% | 30-35% | 40-45% | 45-50% |

**Conservative estimate**: 2x improvement (20% → 40%)
**Optimistic estimate**: 2.5x improvement (20% → 50%)

**Target**: >43.2% to beat Claude Code baseline

## Validation Strategy

1. Run 10 tasks with current implementation (baseline: 20%)
2. Implement Phase 1 fixes
3. Run same 10 tasks (expect: 30-35%)
4. Implement Phase 2 fixes
5. Run same 10 tasks (expect: 40-45%)
6. Full 55-task evaluation

## References

- `/Users/nick/github/nijaru/aircher/ai/research/agent-scaffolding.md` - SWE-Agent patterns
- `/Users/nick/github/nijaru/aircher/ai/research/tui-agents-sota-2025.md` - Feature comparison
- Claude Code architecture research (session summary)
- SWE-Agent paper: https://arxiv.org/abs/2405.15793
