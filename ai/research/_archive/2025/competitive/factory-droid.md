# Factory Droid Architecture Analysis

**Research Date**: 2025-11-20
**Source**: User insider knowledge + competitive analysis
**Benchmark**: 58% Terminal-Bench accuracy (beats everyone)

---

## Key Insight: Spec-First Autonomous Loop

Factory Droid doesn't "chat" - it **plans, executes, validates, self-corrects** without human intervention.

### Architecture Pattern

```
User Task: "Add user authentication"
    ↓
1. SPEC GENERATION (YAML)
   Generate complete execution plan with:
   - Steps to execute
   - Expected outputs
   - Validation criteria
   - Success conditions
    ↓
2. SPEC VALIDATION
   Before coding, verify:
   - All requirements covered
   - Steps are executable
   - Success criteria measurable
    ↓
3. AUTONOMOUS EXECUTION LOOP
   For each step:
     Execute → Run Tests → Capture Errors
     ↓
     If failed: Self-correct spec + retry
     If passed: Next step
    ↓
4. VERIFICATION
   Run full test suite
   Verify all requirements met
```

### Why It Scores 58% on Terminal-Bench

**Traditional agents**:
```python
# Ask LLM each turn
thought = "I should add authentication"
action = "Write auth.py"
# ❌ No validation before moving on
```

**Factory Droid**:
```python
# Generate spec FIRST
spec = {
  "steps": [
    {"action": "Create auth.py", "test": "python auth.py --version"},
    {"action": "Add login endpoint", "test": "curl /login returns 401"},
    {"action": "Add session management", "test": "curl /profile with token works"}
  ],
  "success_criteria": "All tests pass + coverage >80%"
}

# Execute with validation
for step in spec.steps:
    while not step.complete:
        execute(step.action)
        result = run_test(step.test)

        if result.failed:
            errors = capture_errors(result)
            step = self_correct(step, errors)  # Update action
            continue  # Retry

        step.complete = True

# Final verification
assert all_tests_pass()
```

**Key difference**: Autonomous self-correction without asking user

---

## Implementation for Aircher

### Phase 1: Add Spec Generation (8-10h)

```python
# terminal_bench_adapter/aircher_agent.py

class TaskSpec(TypedDict):
    description: str
    steps: list[dict]
    validation_commands: list[str]
    success_criteria: str

def _generate_task_spec(self, task: str) -> TaskSpec:
    """Generate execution spec before coding"""

    prompt = f"""
    Generate a detailed execution plan for this task:
    {task}

    Return JSON with:
    - steps: List of actions to take
    - validation_commands: Commands to verify each step
    - success_criteria: How to know task is complete

    Example:
    {{
      "steps": [
        {{
          "action": "Create user model",
          "files": ["models/user.py"],
          "validation": "python -c 'from models.user import User; print(User.__name__)'"
        }},
        {{
          "action": "Add authentication endpoint",
          "files": ["api/auth.py"],
          "validation": "pytest tests/test_auth.py -v"
        }}
      ],
      "success_criteria": "All tests pass (pytest -v returns 0)"
    }}
    """

    spec_response = self.model.complete(prompt)
    spec = json.loads(spec_response)

    return spec

def _validate_spec_completeness(self, spec: TaskSpec, task: str) -> bool:
    """Verify spec covers all requirements"""

    prompt = f"""
    Task: {task}
    Generated Spec: {spec}

    Does this spec completely address the task?
    Check:
    - All requirements covered
    - Steps are executable
    - Validation is measurable

    Return: {{"complete": true/false, "missing": ["list", "of", "gaps"]}}
    """

    validation = json.loads(self.model.complete(prompt))
    return validation["complete"]
```

### Phase 2: Autonomous Execution Loop (4-6h)

```python
def _execute_with_spec(self, spec: TaskSpec) -> bool:
    """Execute spec with autonomous correction"""

    for step_idx, step in enumerate(spec["steps"]):
        max_retries = 3
        retry_count = 0

        while retry_count < max_retries:
            # Execute step
            result = self._execute_step(step)

            # Validate IMMEDIATELY
            validation = self._validate_step(step, result)

            if validation["success"]:
                self._logger.info(f"Step {step_idx+1} complete")
                break  # Move to next step

            # Self-correct
            self._logger.warn(f"Step {step_idx+1} failed, self-correcting...")
            step = self._self_correct_step(step, validation["errors"])
            retry_count += 1

        if retry_count >= max_retries:
            return False  # Task failed

    # Final verification
    return self._verify_completion(spec)

def _self_correct_step(self, step: dict, errors: list[str]) -> dict:
    """Update step based on errors (NO user input)"""

    prompt = f"""
    Step failed: {step}
    Errors: {errors}

    Generate corrected step that fixes these errors.
    Return updated step JSON.
    """

    corrected = json.loads(self.model.complete(prompt))
    return corrected
```

### Phase 3: Integration with Current Loop (2-3h)

```python
# Replace current loop
def perform_task(self, task: str):
    # Generate spec FIRST
    spec = self._generate_task_spec(task)

    # Validate spec completeness
    if not self._validate_spec_completeness(spec, task):
        spec = self._refine_spec(spec, task)

    # Execute with autonomous correction
    success = self._execute_with_spec(spec)

    return success
```

---

## Expected Impact

**Baseline** (v13): TBD (running)

**After Spec-First** (v14):
- Expected: +10-15pp
- Reason: Autonomous self-correction eliminates iteration waste
- Target: 45-50% accuracy (approaching Factory Droid's 58%)

**Key Metrics**:
- Iterations saved per task: 5-10 (no retry loops)
- Self-corrections: Track how many times spec is updated
- Validation success rate: % of validations that pass first try

---

## Differences from Current Approach

| Current Aircher | Factory Droid Pattern |
|-----------------|----------------------|
| Think → Act → Observe | Spec → Execute → Validate → Correct |
| Each turn generates new plan | Plan generated ONCE upfront |
| LLM decides when complete | Spec defines success criteria |
| Manual verification | Autonomous validation loop |
| No self-correction | Self-corrects on failure |

---

## Risks

1. **Spec generation overhead**: First turn takes longer
   - Mitigation: Cache specs for similar tasks

2. **Over-specification**: Too detailed specs may be brittle
   - Mitigation: Generate high-level specs, refine as needed

3. **Validation false positives**: Test passes but task incomplete
   - Mitigation: Multi-level validation (unit tests + integration tests)

---

## Success Criteria for Implementation

**Phase 1 Complete** (Spec generation):
- ✅ Can generate valid TaskSpec from task description
- ✅ Spec includes actionable steps + validation commands
- ✅ Validation catches incomplete specs

**Phase 2 Complete** (Autonomous loop):
- ✅ Self-correction works without user input
- ✅ Retry logic prevents infinite loops
- ✅ Steps validate BEFORE moving on

**Phase 3 Complete** (Integration):
- ✅ Terminal-Bench accuracy >45% (spec-first approach)
- ✅ Iteration count reduced by 30%+
- ✅ No regressions on tasks that already passed

---

## Timeline

- Week 1 (8-10h): Spec generation + validation
- Week 1 (4-6h): Autonomous execution loop
- Week 1 (2-3h): Integration with current adapter
- **Total**: 14-19h implementation
- **Testing**: 5-10h (run benchmarks, iterate)

**Target**: v15 with spec-first pattern, 45-50% accuracy

---

## References

- Terminal-Bench leaderboard: Factory Droid 58.8% (current leader)
- User insight: "Spec-First Architecture" explanation
- Related: Codex improvement roadmap (planning patterns)
