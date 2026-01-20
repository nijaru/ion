# Codex CLI Architecture Analysis
## Key Patterns for Terminal-Bench Performance (57.8% Accuracy)

**Research Date**: 2025-11-17
**Source**: `/Users/nick/github/openai/codex` - Production OpenAI agent
**Reference Accuracy**: 57.8% Terminal-Bench
**Our Current Accuracy**: 40% (v6 - approaching target!)

---

## 1. MAIN LOOP ARCHITECTURE

### Core Pattern: Task-Based Workflow (codex.rs:1763-1899)

```
run_task() → Loop {
  1. Get pending user input
  2. Build conversation history for prompt
  3. run_turn() → stream from model → handle items
  4. Process responses:
     - Empty responses → break (task complete)
     - Token limit exceeded → auto-compact & retry
     - Error → surface to user & break
  5. Loop back for next turn
}
```

**Key Insight**: **Multi-turn task architecture** with explicit "end of task" detection.

- **Not** a simple single-turn request/response
- **Not** a fixed iteration count (no premature stopping)
- **Continues iterating** until model produces no more responses
- **Token limit aware**: Proactively compacts if exceeding budget

### Retry/Error Recovery (codex.rs:1927-1992)

**Stream Retry Logic** - Handles transient network failures:
```rust
match try_run_turn(...) {
    Ok(output) => return Ok(output),
    Err(CodexErr::Stream(_, Some(delay))) => {
        retries < max_retries → sleep(delay) → retry
    }
    Err(other) => backoff(retries) → retry
}
```

**Max retries**: Provider-specific (configurable)
**Backoff strategy**: Exponential with jitter (util::backoff)

**Auto-Compaction Loop** (codex.rs:1842-1858):
- Monitors token usage in real-time
- If hitting auto-compact limit (configurable):
  1. Auto-compact conversation history
  2. Continue turn (user doesn't know)
  3. If STILL over limit after compact → surface error

**Critical Behavior**: Handles context window gracefully - doesn't kill agent mid-task.

---

## 2. TOOL ORCHESTRATION PATTERNS

### Router + Registry (tools/router.rs, tools/registry.rs)

**Three-stage dispatch**:

```
ToolRouter::dispatch_tool_call()
  ↓
1. Build ToolCall from ResponseItem
   - Distinguishes: Function call vs Custom tool vs MCP tool
   - Extracts call_id, payload, arguments
  ↓
2. ToolRegistry::dispatch(ToolInvocation)
   - Routes to appropriate handler
   - Handles MCP tools (external servers)
  ↓
3. Failure handling
   - Fatal errors → propagate
   - Recoverable errors → ResponseInputItem failure message → model gets feedback
```

**ToolOrchestrator Pattern** (tools/orchestrator.rs):

The **Approval + Sandbox + Retry** workflow:

```
1. APPROVAL PHASE
   - Check if tool needs initial approval (config-driven)
   - Get sandbox risk assessment (optional)
   - Show user decision request
   - Cache approval for this session

2. EXECUTION PHASE (Initial)
   - Select sandbox (None/Restricted/Full)
   - Execute tool
   - If success → return result
   - If sandbox denied (restriction hit):
     - Check escalate_on_failure flag
     - Request re-approval for unboxed attempt
     - Re-execute without sandbox

3. RETRY DECISION
   - Should tool retry on failure?
   - Should retry require re-approval?
   - Config-driven decisions
```

**Key Pattern**: **Approval caching** - only ask once per session, escalate on failure.

### Parallel Tool Execution (tools/parallel.rs)

Uses `FuturesOrdered` to:
- Launch all tool calls from single response
- Await completion in **order issued**
- Handle mixed serial/parallel (some tools don't support parallel)

**Per-tool configuration**: `supports_parallel_tool_calls` flag

---

## 3. VERIFICATION & VALIDATION PATTERNS

### Review Task Pattern (tasks/review.rs)

**Sub-agent for code review**:

```
ReviewTask::run()
  ↓
1. Start sub-codex conversation (separate session)
   - Disable certain features (web search, image viewing)
   - Load REVIEW_PROMPT as system prompt
   - Run one turn only
  ↓
2. Process review events
   - Suppress item_completed for assistant messages
   - Capture last_agent_message
   ↓
3. Parse ReviewOutputEvent
   - Try strict JSON parse
   - Fallback: extract {...} substring
   - Final fallback: wrap plain text
  ↓
4. Exit review mode
   - Emit ExitedReviewMode event
   - Record review findings in history
```

**REVIEW_PROMPT** (review_prompt.md):

Structured review criteria with **priority levels** [P0-P3]:
- P0: Blocking issues (must fix)
- P1: Urgent (next cycle)
- P2: Normal (eventually)
- P3: Nice-to-have

Output schema:
```json
{
  "findings": [
    {
      "title": "...",
      "body": "...",
      "priority": 1,
      "code_location": {"absolute_file_path": "...", "line_range": {...}}
    }
  ],
  "overall_correctness": "patch is correct" | "patch is incorrect",
  "overall_explanation": "..."
}
```

**Key Insight**: Review is **deterministic + structured** (not open-ended).

---

## 4. CONTEXT MANAGEMENT

### Conversation History Architecture

**SessionState** stores:
- Full conversation history (ResponseItems)
- Token usage info (input_tokens, output_tokens, cache_tokens)
- Rate limit snapshots
- Session configuration (model, cwd, permissions)

**History Retrieval** (codex.rs:1804):
```rust
history.get_history_for_prompt() → Vec<ResponseItem>
```

Includes:
- All previous user messages
- All previous assistant responses
- All tool execution results
- Turn context (cwd, model, permissions)

### Compaction Strategy (compact.rs)

**When to compact**:
- Explicit user request (Op::Compact)
- Auto-compact when token limit approaches (configurable)
- Ghost snapshot during execution

**Compaction process**:

```
1. Clone current history
2. Loop:
   a. Build prompt with current history
   b. Stream completion → summary + cleaned items
   c. If success → persist, break
   d. If token limit → truncate oldest items → retry
   e. Retry up to max_retries
3. Update session history
```

**Key Behavior**:
- Does NOT interrupt main task
- Inline compaction (user continues conversation)
- Graceful degradation (truncate if can't fit)

### Token Tracking (codex.rs:1106-1114)

```rust
pub async fn set_total_tokens_full(&self, turn_context: &TurnContext) {
    context_window = turn_context.client.get_model_context_window()
    state.set_token_usage_full(context_window)
    send_token_count_event() // Notify UI
}
```

**Real-time updates** sent to client after each turn.

---

## 5. PLANNING & DECOMPOSITION

### Task Types (tasks/mod.rs)

Codex supports **multiple task kinds**, each with different strategies:

- **RegularTask**: Standard chat/coding
- **ReviewTask**: Code review (sub-agent)
- **CompactTask**: Conversation summarization
- **UndoTask**: Revert changes
- **UserShellCommandTask**: Direct shell execution
- **GhostSnapshotTask**: Parallel commit tracking

**Each task** implements SessionTask trait:
```rust
pub trait SessionTask {
    fn kind(&self) -> TaskKind;
    async fn run(...) -> Option<String>;  // Final message
    async fn abort(...);                   // Cleanup
}
```

### Planning in Prompts

**Base Instructions** (codex-rs/core/prompt.md, gpt_5_codex_prompt.md):

Large system prompts (24KB+) with:
1. **Tool descriptions** - exact signatures, examples
2. **Planning guidance** - think-act-observe loop
3. **Safety constraints** - what NOT to do
4. **Best practices** - successful patterns

**Model-specific instructions**:
- GPT-5.1 vs GPT-5 Codex get different prompts
- Apply-patch tool has special instructions (tool version dependent)

**Custom prompts**: Per-project instructions (from .codexrc files)

---

## 6. ERROR HANDLING & RECOVERY

### Error Type Hierarchy (error.rs)

```rust
pub enum CodexErr {
    TurnAborted { dangling_artifacts },      // User interrupt
    Interrupted,                               // Cancellation token
    EnvVar(String),                           // Missing env
    Fatal(String),                            // Unrecoverable
    ContextWindowExceeded,                    // Token limit
    UsageLimitReached(RateLimitError),       // API quota
    QuotaExceeded,                            // Account limit
    RefreshTokenFailed(String),               // Auth failure
    Stream(String, Option<Duration>),        // Network error
    // ... ~20 more error types
}
```

### Specific Recovery Paths (codex.rs:1940-1992)

Each error type has tailored recovery:

```rust
Err(CodexErr::TurnAborted { dangling_artifacts })
    → Process dangling artifacts → Break (user-requested)

Err(CodexErr::Interrupted)
    → Return immediately (cancellation token fired)

Err(CodexErr::ContextWindowExceeded)
    → set_total_tokens_full() → Surface error

Err(CodexErr::UsageLimitReached(e))
    → Update rate limits → Surface error

Err(CodexErr::Stream(...))
    → retries < max_retries → backoff → retry
    → else surface error
```

### Graceful Degradation

**Three-level approval system** (orchestrator.rs):

1. **Initial approval** (can tool be used at all?)
2. **Sandbox denial** (policy blocked execution)
3. **Escalation approval** (allow without sandbox?)

Each level can be:
- Auto-approved (config)
- User-requested
- Cached from session
- Re-evaluated on failure

---

## 7. KEY TECHNIQUES FOR 57.8% ACCURACY

### A. Structured Iteration Loop

**Instead of**: Fixed N iterations
**Codex does**: Continue until model stops producing responses

Benefits:
- Simple tasks finish faster
- Complex tasks get more iterations (naturally)
- No premature stopping

### B. Sub-Agent Pattern

**Code Review as separate agent**:
- Removes conflict between "build code" and "critique code"
- Review has stripped-down tools (no web, no images)
- Runs with REVIEW_PROMPT (not main instructions)
- Results structured + parseable

**Extensible**: Add specialized sub-agents for:
- Optimization review
- Security audit
- Performance analysis

### C. Approval Caching

Users approve **once per session**, escalate on failure
- Reduces friction
- Still maintains control
- Tool runtime learns preferences

### D. Task-Aware History

Build different prompts based on:
- Current cwd (workspace context)
- Approval policy (what tools are allowed)
- Sandbox policy (what restrictions apply)
- Model reasoning effort (spend more tokens on hard tasks?)

### E. Real-Time Token Monitoring

**Proactive**: Know token usage BEFORE hitting limit
**Embedded in prompts**: "Token usage: X/128000; Y remaining"
**Auto-action**: Compact before hitting limit

### F. Error Recovery with Context

When tools fail:
1. **Dont hide it** - show full error output
2. **Dont retry blindly** - analyze why it failed
3. **Ask for escalation** - "This failed in sandbox; allow retry unboxed?"
4. **Learn from it** - Track failures in episodic memory

### G. Multi-Turn Task Loop

Instead of:
```
1. User input
2. Model response
3. Return
```

Codex does:
```
Loop:
  1. Get pending input
  2. Build history
  3. Stream from model
  4. Handle tools
  5. More responses?
     → Yes: continue
     → No: done
```

**User can interrupt mid-task** without losing context.

---

## 8. IMPLEMENTATION RECOMMENDATIONS FOR AIRCHER

Based on Codex patterns + our current 40% accuracy:

### IMMEDIATE (High ROI)

**1. Multi-turn completion loop** (like codex.rs:1789-1896)
```python
while True:
    response = await model.stream(conversation)
    if response.is_empty():
        break
    # Process items, update history
```
**Effort**: Medium | **Impact**: +5-10% (finish tasks naturally)

**2. Auto-compaction on token limit** (like compact.rs)
```python
if token_usage > auto_compact_threshold:
    await compact_conversation()
    # Continue task automatically
```
**Effort**: Medium | **Impact**: +2-5% (handle long tasks)

**3. Structured error recovery** (like orchestrator.rs)
- Tool fails → Analyze failure → Request escalation
- Tool succeeds → Update memory
- Build decision tree per tool
**Effort**: Medium | **Impact**: +3-5% (smarter retries)

### MEDIUM TERM (Scaling)

**4. Sub-agent pattern** (like review.rs)
- Verification agent: runs separate turn to verify completion
- Planning agent: decompose complex tasks
- Debug agent: analyze failures
**Effort**: High | **Impact**: +5-15% (specialized reasoning)

**5. Task-specific prompts** (like model_family.rs)
- Different instructions for coding vs debugging vs verification
- Conditional tool availability
- Custom examples per task type
**Effort**: Medium | **Impact**: +3-8% (focused reasoning)

**6. Approval caching + escalation** (like tools/orchestrator.rs)
- Cache user decisions across turns
- Escalate on failure (allow risky operations if safer method fails)
**Effort**: Low | **Impact**: +2-3% (faster iteration)

### LONG TERM

**7. Sub-task tracking** (like GhostSnapshotTask)
- Parallel verification during main task
- Early warning of failures
**Effort**: High | **Impact**: +3-5% (catch issues early)

---

## 9. ARCHITECTURAL INSIGHTS

### Why Codex Achieves 57.8%

1. **Multi-turn loop** - Tasks continue naturally until done
2. **Error as learning** - Failures shown to model for recovery
3. **Structured verification** - Review step validates completion
4. **Tool orchestration** - Clear approval + sandbox + retry flow
5. **Context awareness** - History tracks reasoning, not just outputs
6. **Adaptive iteration** - No fixed limits, task-driven duration
7. **Real-time monitoring** - Proactive compaction + token tracking

### Our Progress (from 20% → 40%)

- ✅ Forced verification (v5)
- ✅ Efficiency rules (v6)
- ✅ SWE-Agent history processor
- ✅ Adaptive planning
- ⏳ Sub-agent pattern (next)
- ⏳ Auto-compaction (next)
- ⏳ Structured error recovery (next)

### Path to >57.8%

Our current **40% with free Sherlock model** is within reach of:
- **Multi-turn loop**: 40% → 45% (+5pp)
- **Auto-compaction**: 45% → 50% (+5pp)
- **Sub-agent verification**: 50% → 55% (+5pp)
- **Task decomposition**: 55% → 60% (+5pp)

**Realistic ceiling without larger model**: ~55-60% with scaffolding only

---

## 10. CODE LOCATIONS & REFERENCES

| Pattern | Location | Lines | Key Code |
|---------|----------|-------|----------|
| Main loop | `codex.rs` | 1763-1899 | `run_task()` - task completion detection |
| Tool dispatch | `tools/router.rs` | 132-165 | `dispatch_tool_call()` - 3-stage dispatch |
| Approval + sandbox | `tools/orchestrator.rs` | 33-179 | `run()` - approval → attempt → escalate |
| Review sub-agent | `tasks/review.rs` | 35-98 | `start_review_conversation()` |
| Token tracking | `codex.rs` | 1106-1114 | `set_total_tokens_full()` |
| Auto-compaction | `compact.rs` | 31-50 | `run_inline_auto_compact_task()` |
| Error hierarchy | `error.rs` | entire | 25+ error types with recovery |
| Retry logic | `codex.rs` | 1927-1992 | Stream retry with backoff |
| Task trait | `tasks/mod.rs` | 68-98 | `SessionTask` - multi-task pattern |
| History management | `context_manager/` | multiple | Conversation history + token tracking |

---

## 11. SPECIFIC TECHNIQUES TO IMPROVE FROM 40%

### High Priority (3.2pp to reach 43.2%)

**A. Multi-turn loop completion detection**
- Current: All tasks run for fixed iterations
- Codex: Continue while model produces responses
- Implementation: Check `response.items.is_empty()` → break

**B. Verify AFTER completion actions**
- Current: Verify before marking complete
- Better: Mark complete, THEN force verification turn
- Impact: Catch self-hallucination (agent says done but hasn't)

**C. Context-aware tool selection**
- Current: All tools available always
- Better: Disable tools based on task type (no web search in offline tasks)
- Impact: Reduce hallucination about unavailable tools

### Medium Priority (for 50%+)

**D. Real-time token monitoring**
- Embed usage: "Token: 45000/128000 (35%)" in prompts
- Auto-compact at 80%
- Prevent context overflow

**E. Structured review prompt**
- Current: Generic "verify completion" prompt
- Better: Task-specific review criteria
- Impact: Better verification accuracy

**F. Escalation on failure**
- Current: Tool fails → try again same way
- Better: Tool fails → request escalation → allow dangerous ops
- Impact: Break out of deadlocks

---

## SUMMARY

**OpenAI Codex achieves 57.8% through**:

1. **Structural**: Multi-turn task loop (not fixed iterations)
2. **Operational**: Clear error recovery with escalation
3. **Tactical**: Sub-agent review pattern
4. **Monitoring**: Real-time token tracking + auto-compaction
5. **Orchestration**: Approval caching + sandbox retry

**We're at 40% with similar scaffolding**. Next 10-15pp available through:
1. Multi-turn completion detection (natural task endings)
2. Verify AFTER action (catch hallucinations)
3. Auto-compaction (handle long tasks)
4. Sub-agent verification (specialized review)
5. Better error context (show failures to model)

**Ceiling without larger model**: ~55-60%
**Path is clear**: Implement Codex patterns → likely reach 50%+
