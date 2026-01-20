# Implementation Roadmap: Codex Patterns for Aircher

**Objective**: Improve from 40% → 50%+ accuracy using proven Codex patterns
**Timeline**: 3-4 optimization cycles
**Current Baseline**: 40% accuracy, 10 tasks, free Sherlock model

---

## PHASE 1: Multi-Turn Completion Detection (40% → 45%)

**Problem**: Tasks stop at fixed iteration limit, don't continue naturally
**Codex Solution**: Check response.is_empty() to detect task completion
**Effort**: Medium (4-6 hours)

### Implementation Steps

1. **Replace fixed-iteration loop** in `aircher_agent.py::_run_agent_loop()`

   **Current**:
   ```python
   for iteration in range(max_iterations):
       # Run one turn
       # Even if task is done, continue to limit
   ```

   **Codex Pattern**:
   ```python
   iteration = 0
   while iteration < max_iterations:
       response = await model.stream(conversation)
       if response.is_empty():
           # Task naturally complete
           break
       # Process response, update history
       iteration += 1
   ```

2. **Define completion criteria**
   - No tool calls in response
   - No new content generated (assistant message empty)
   - Task reaches success state (based on memory tracking)

3. **Add "task end" signal to prompts**
   ```python
   # In system prompt:
   "If the task is complete, respond with EMPTY MESSAGE.
    Do not call any tools. This signals task completion."
   ```

4. **Test with simple tasks first**
   - hello-world (should complete in 1-2 iterations)
   - fix-pandas-version (should complete in 2-3 iterations)
   - Verify iterations decrease for simple tasks

**Expected Impact**: +5-10pp
**Risk**: Low (compatible with existing structure)

---

## PHASE 2: Auto-Compaction on Token Limit (45% → 48%)

**Problem**: Long tasks hit context window, fail ungracefully
**Codex Solution**: Monitor tokens, compact before hitting limit
**Effort**: Medium (6-8 hours)

### Implementation Steps

1. **Add token tracking**
   ```python
   # In TurnContext or Session
   def get_token_usage(response):
       return {
           'input_tokens': response.usage.input_tokens,
           'output_tokens': response.usage.output_tokens,
           'total': response.usage.input_tokens + response.usage.output_tokens
       }

   def check_needs_compaction(total_tokens, context_window=128000):
       threshold = int(context_window * 0.80)  # Compact at 80%
       return total_tokens >= threshold
   ```

2. **Implement compaction trigger**
   ```python
   async def _maybe_compact_history(self):
       usage = self.get_accumulated_tokens()
       if usage['total'] > compaction_threshold:
           logger.info(f"Token limit approaching ({usage['total']}/{context_window})")
           # Trigger compaction
           await self._compact_conversation()
           # Continue task automatically
   ```

3. **Implement conversation compaction**
   ```python
   async def _compact_conversation(self):
       # Get current history
       history = self.conversation_history

       # Create compaction prompt
       compact_prompt = f"""
       Summarize this conversation, preserving key decisions and context.
       Keep reasoning about why choices were made.
       Elide old observations with [... N lines elided].

       {history}
       """

       # Call model for summary
       summary = await self.model.complete(compact_prompt)

       # Replace history with summary
       self.conversation_history = summary
       self.tokens_since_compact = 0
   ```

4. **Add token display in prompts**
   ```python
   # Include in every prompt:
   f"[Token usage: {usage['total']}/{context_window} ({usage['total']/context_window*100:.0f}%)]"
   ```

5. **Test on long tasks**
   - rust-heap-lzma (known to hit token limits)
   - hydro-data-join (spent 9 iterations, should compact)
   - Verify compaction triggers and task continues

**Expected Impact**: +3pp (prevents context overflow)
**Risk**: Low (isolated from main loop)

---

## PHASE 3: Structured Error Recovery (48% → 51%)

**Problem**: Tool failures cause agent to retry blindly or give up
**Codex Solution**: Escalation pattern - try harder with approval
**Effort**: Medium (8-10 hours)

### Implementation Steps

1. **Build tool failure analyzer**
   ```python
   class ToolFailureAnalyzer:
       def analyze(self, tool_name: str, error: str) -> FailureAnalysis:
           return {
               'category': self.categorize(error),  # Sandbox, NotFound, PermissionDenied, etc
               'recoverable': self.is_recoverable(error),
               'escalation_needed': self.needs_escalation(error),
               'suggestion': self.suggest_fix(error)
           }
   ```

2. **Implement escalation decision tree**
   ```python
   async def handle_tool_failure(self, call_id: str, error: str):
       analysis = self.failure_analyzer.analyze(tool_name, error)

       if analysis['escalation_needed']:
           # Try without restrictions
           logger.info(f"Requesting escalation for {tool_name}: {analysis['suggestion']}")
           escalation_granted = await self._request_escalation(analysis)

           if escalation_granted:
               # Retry with escalated permissions
               result = await self._execute_tool(call_id, tool_name, escalated=True)
           else:
               # Return error for model to see
               return {'status': 'failed', 'error': error}
       else:
           # Suggest alternative approach
           suggestion = analysis['suggestion']
           # Include in prompt for model
   ```

3. **Add memory tracking for failures**
   ```python
   # In memory system:
   async def record_tool_failure(self, tool_name: str, reason: str):
       await self.memory.track_tool_failure(
           tool_name=tool_name,
           reason=reason,
           timestamp=time.time()
       )

   # Later: Check if tool repeatedly fails
   failure_count = await self.memory.get_failure_count(tool_name)
   if failure_count > 3:
       logger.warn(f"{tool_name} failed {failure_count} times, suggest alternative")
   ```

4. **Include failure context in prompts**
   ```python
   # In system prompt for failed commands:
   f"Previous attempt failed with: {error}\n" \
   f"Consider: {suggestion}\n" \
   f"This tool has failed {failure_count} times in this session."
   ```

5. **Test with sandbox-restrictive tasks**
   - aws-bucket-creation (likely hits ACL restrictions)
   - build-shell-script (might hit permission errors)
   - Verify escalation flow works

**Expected Impact**: +3pp (better recovery from failures)
**Risk**: Medium (requires error classification accuracy)

---

## PHASE 4: Sub-Agent Verification (51% → 55%)

**Problem**: Agent marks task complete without proper verification
**Codex Solution**: Separate verification sub-agent with review prompt
**Effort**: High (12-16 hours)

### Implementation Steps

1. **Create verification sub-agent class**
   ```python
   class VerificationAgent:
       def __init__(self, model_provider, base_instructions):
           self.model = model_provider
           # Load review-specific prompt
           self.review_prompt = self._load_review_prompt()

       async def verify_task_completion(
           self,
           task_description: str,
           completion_evidence: str,
           test_results: str
       ) -> VerificationResult:
           """Run verification turn"""

           prompt = f"""
           {self.review_prompt}

           TASK: {task_description}

           COMPLETION EVIDENCE:
           {completion_evidence}

           TEST RESULTS:
           {test_results}

           Verify: Does the completion satisfy all requirements?
           """

           response = await self.model.complete(prompt)
           return self._parse_verification_result(response)
   ```

2. **Define review criteria structured prompt**
   ```python
   VERIFICATION_PROMPT = """
   You are verifying whether a task has been completed successfully.

   CRITERIA:
   1. All requirements from the task description are met
   2. Solution is correct (test suite passes, if applicable)
   3. No errors or warnings in output
   4. Solution is production-ready (or meets stated quality level)
   5. No obvious bugs or edge cases

   Return JSON with:
   {
       "is_complete": bool,
       "confidence": float (0.0-1.0),
       "issues": [
           {
               "issue": "description",
               "severity": "critical|major|minor",
               "suggestion": "how to fix"
           }
       ],
       "summary": "1-line verdict"
   }
   """
   ```

3. **Integrate verification into main loop**
   ```python
   async def _run_agent_loop(self):
       iteration = 0
       while iteration < max_iterations:
           # ... normal loop ...

           if agent_signal_complete:
               # Run verification AFTER completion signal
               logger.info("Agent claims completion, verifying...")
               verification = await self.verifier.verify_task_completion(
                   task_description=self.task,
                   completion_evidence=self._collect_evidence(),
                   test_results=self._run_tests() if applicable
               )

               if verification['is_complete']:
                   logger.info(f"Verified complete (confidence: {verification['confidence']})")
                   break
               else:
                   # Issues found, include in prompt for next iteration
                   logger.warn(f"Verification failed: {verification['issues']}")
                   # Prompt agent to fix issues
   ```

4. **Collect completion evidence**
   ```python
   def _collect_evidence(self) -> str:
       """Gather evidence of task completion"""
       return {
           'final_terminal_output': self.terminal_output[-500:],  # Last 500 lines
           'created_files': self.memory.get_created_files(),
           'successful_commands': self.memory.get_successful_commands()[-10:],  # Last 10
           'git_diff': self.memory.get_git_diff() if applicable
       }
   ```

5. **Test with verification-sensitive tasks**
   - csv-to-organization-json (needs output validation)
   - parse-hexdump-octal (needs byte-level validation)
   - Verify sub-agent catches incomplete solutions

**Expected Impact**: +4pp (catch hallucinations early)
**Risk**: High (requires separate model call per task)

---

## PHASE 5: Task-Specific Prompts (55% → 58%)

**Problem**: One prompt size doesn't fit all task types
**Codex Solution**: Model-specific instructions, task-specific examples
**Effort**: High (16-20 hours)

### Implementation Steps

1. **Classify task types**
   ```python
   class TaskClassifier:
       def classify(self, task_description: str) -> TaskType:
           if 'install' in task or 'apt-get' in task:
               return TaskType.ENVIRONMENT_SETUP
           elif 'write' in task or 'create' in task:
               return TaskType.CODE_GENERATION
           elif 'fix' in task or 'debug' in task:
               return TaskType.DEBUGGING
           elif 'test' in task or 'verify' in task:
               return TaskType.TESTING
           else:
               return TaskType.GENERAL
   ```

2. **Create task-specific prompt templates**
   ```python
   PROMPTS = {
       TaskType.ENVIRONMENT_SETUP: """
           You are setting up a development environment.

           APPROACH:
           1. Check current state (tool versions, dependencies)
           2. Plan installation sequence (respect dependencies)
           3. Install requirements (use apt, pip, etc.)
           4. Verify installation (run test command)

           TOOLS: shell, read_file
           DO NOT: Run code before verifying dependencies
       """,

       TaskType.CODE_GENERATION: """
           You are writing code to solve a problem.

           APPROACH:
           1. Understand requirements (read task carefully)
           2. Check existing code (if modifying)
           3. Write solution
           4. Test thoroughly

           TOOLS: write_file, shell, grep
           DO NOT: Hallucinate library APIs
       """,
       # ... more task types
   }
   ```

3. **Build model-family specific variants**
   ```python
   MODEL_VARIANTS = {
       'claude-3-5-sonnet': {
           'suffix': "Use advanced reasoning. Take time to understand deeply.",
           'examples': [...more complex examples...]
       },
       'sherlock-think': {
           'suffix': "This model supports extended thinking. Use it for complex analysis.",
           'examples': [...suited to thinking model...]
       },
       'gpt-4o': {
           'suffix': "Focus on practical, working solutions.",
           'examples': [...pragmatic examples...]
       }
   }
   ```

4. **Include task-specific examples**
   ```python
   # Examples format:
   [
       {
           "task": "Fix Python 2 to Python 3 compatibility",
           "approach": "1. Identify Python 2 code 2. Use 2to3 tool 3. Fix remaining issues",
           "tools_order": ["grep", "shell", "write_file"],
           "success_patterns": ["ModuleNotFoundError fixed", "All tests pass"]
       },
       # ... more examples
   ]
   ```

5. **Load and inject into prompts**
   ```python
   async def build_prompt(self, task: str) -> str:
       task_type = self.classifier.classify(task)
       base_prompt = PROMPTS[task_type]
       model_variant = MODEL_VARIANTS[self.model_family]
       examples = EXAMPLES[task_type]

       return f"""
       {base_prompt}

       MODEL INSTRUCTIONS: {model_variant['suffix']}

       EXAMPLES FOR THIS TASK TYPE:
       {self._format_examples(examples)}

       YOUR TASK: {task}
       """
   ```

6. **Test with diverse task set**
   - Run all 10 tasks
   - Verify task-specific prompts improve accuracy for each type
   - Compare vs generic prompt

**Expected Impact**: +3pp (focused reasoning)
**Risk**: High (requires prompt engineering per task type)

---

## INTEGRATION CHECKLIST

Before deploying each phase:

- [ ] Unit tests pass (existing test suite)
- [ ] Integration test with 5+ tasks
- [ ] Memory system still tracks correctly
- [ ] Error handling doesn't hide failures
- [ ] Token counting accurate
- [ ] No infinite loops
- [ ] Performance acceptable (<5s per iteration)

---

## MEASUREMENT STRATEGY

Run Terminal-Bench after each phase:
- **Baseline**: 40% (v6, current)
- **After Phase 1**: Target 45% (multi-turn completion)
- **After Phase 2**: Target 48% (auto-compaction)
- **After Phase 3**: Target 51% (error recovery)
- **After Phase 4**: Target 55% (sub-agent verification)
- **After Phase 5**: Target 58%+ (task-specific prompts)

Each test: n=10 tasks, report accuracy + per-task results

---

## ESTIMATED IMPACT

| Phase | Effort | Risk | Impact | Cumulative |
|-------|--------|------|--------|-----------|
| 1: Multi-turn | 4-6h | Low | +5pp | 45% |
| 2: Auto-compact | 6-8h | Low | +3pp | 48% |
| 3: Error recovery | 8-10h | Medium | +3pp | 51% |
| 4: Sub-agent verification | 12-16h | High | +4pp | 55% |
| 5: Task-specific prompts | 16-20h | High | +3pp | 58% |

**Total effort**: ~46-60 hours
**Expected final accuracy**: 55-58% (beating Codex 57.8%)

---

## FALLBACK PLAN

If any phase doesn't deliver expected impact:
1. Revert changes
2. Analyze failure (debug, trace, profile)
3. Try alternative Codex pattern
4. Document learnings in ai/decisions/

Priority order for alternatives:
1. Reasoning effort tuning (maybe model needs more tokens to think?)
2. Context compression (different summarization strategy)
3. Tool set expansion (add web search for research tasks)
4. Larger model evaluation (Claude 3.5 Sonnet vs Sherlock)
