# Adaptive Spec Pattern: Beyond Factory Droid

**Research Date**: 2025-11-21
**Target**: >60% Terminal-Bench (beat Factory Droid's 58.8%)
**Key Insight**: Leverage unique advantages Factory Droid doesn't have

---

## Factory Droid Pattern (Current SOTA: 58.8%)

```
Task → Spec → Execute → Validate → Retry
```

**Strengths**:
- Plan-driven execution (no drift)
- Autonomous self-correction
- Measurable success criteria

**Weaknesses**:
1. **Static spec**: Generated once, only updates on failure
2. **No memory**: Each task starts fresh, no learning
3. **Blind retry**: Self-correction without context
4. **No codebase awareness**: Spec ignores existing structure

---

## Our Unique Advantages

| Capability | Factory Droid | Aircher |
|------------|---------------|---------|
| Memory | None | 3-layer (DuckDB + ChromaDB + KG) |
| Pre-validation | None | LSP auto-fix |
| Codebase structure | None | Knowledge Graph (tree-sitter) |
| Similar code search | None | Hybrid search (regex + semantic) |
| Past failures | None | Episodic memory patterns |

---

## Adaptive Spec Pattern (Target: 62-68%)

```
Task → Memory Query → Informed Spec → Pre-Validate → Execute → Learn → Adapt
```

### Phase 1: Memory-Informed Spec Generation

**Why better than Factory Droid**:
Factory Droid generates specs blind. We query memory first.

```python
def _generate_informed_spec(self, task: str) -> TaskSpec:
    """Generate spec with full context from memory systems."""

    # Query Layer 1: Similar tasks from episodic memory
    similar_tasks = self.episodic.query_similar_tasks(task, limit=3)
    past_solutions = [t["solution_approach"] for t in similar_tasks]

    # Query Layer 2: Relevant code from vector search
    relevant_code = self.vector.search(task, top_k=10)

    # Query Layer 3: Codebase structure from knowledge graph
    structure = self.kg.get_relevant_structure(task)
    # Returns: related functions, imports, file dependencies

    # Query Layer 4: Past failure patterns
    failure_patterns = self.episodic.get_failure_patterns(task_type)
    # Returns: errors to avoid, failed approaches

    # Generate spec with full context
    spec = self.llm.generate_spec(
        task=task,
        similar_solutions=past_solutions,
        relevant_code=relevant_code,
        codebase_structure=structure,
        avoid_patterns=failure_patterns
    )

    return spec
```

**Expected Impact**: +5-8pp over blind spec generation
- Fewer invalid plans (know what exists)
- Avoid repeated failures (memory of what failed)
- Better validation criteria (based on similar tasks)

---

### Phase 2: LSP Pre-Validated Execution

**Why better than Factory Droid**:
Factory Droid: Execute → Fail → Retry
Adaptive: Validate → Fix → Execute (skip failures)

```python
def _execute_step_with_lsp(self, step: StepSpec) -> StepResult:
    """Execute with LSP pre-validation (catches errors before running)."""

    # Extract code from step
    code_blocks = self._extract_code_blocks(step.action)

    for code in code_blocks:
        # PRE-VALIDATE before execution
        lsp_result = self.lsp.validate_and_fix(code.path, code.content)

        if lsp_result["has_errors"]:
            # Fix BEFORE execution - no retry loop needed
            code.content = lsp_result["fixed_content"]
            self._logger.info(f"LSP fixed {len(lsp_result['errors'])} errors pre-execution")

        elif lsp_result["auto_fixes_available"]:
            # Apply formatting/style fixes
            code.content = lsp_result["fixed_content"]

    # Execute with validated code (higher success rate)
    result = self._execute_command(step.command_with_fixed_code)

    return result
```

**Expected Impact**: +3-5pp
- 30% fewer retry loops (errors caught before execution)
- Faster iteration (no failed executions to debug)
- Cleaner code output (auto-formatted)

---

### Phase 3: Adaptive Spec Refinement

**Why better than Factory Droid**:
Factory Droid: Static spec, only corrects on failure
Adaptive: Spec evolves with new knowledge

```python
def _execute_with_adaptation(self, spec: TaskSpec) -> bool:
    """Execute spec with adaptive refinement after each step."""

    accumulated_knowledge = []

    for i, step in enumerate(spec.steps):
        # Execute step
        result = self._execute_step_with_lsp(step)

        # Extract learnings from execution
        learnings = self._extract_learnings(result)
        # Returns: new files discovered, APIs found, errors encountered
        accumulated_knowledge.append(learnings)

        if result.success:
            # REFINE remaining spec with new knowledge
            remaining_steps = spec.steps[i+1:]
            spec.steps[i+1:] = self._refine_remaining_steps(
                remaining_steps,
                accumulated_knowledge,
                self.kg.get_updated_view()  # KG updated with new files
            )
            self._logger.info(f"Refined {len(remaining_steps)} remaining steps")

        else:
            # Memory-informed self-correction
            corrected_step = self._memory_informed_correction(step, result)
            # ... retry logic

    return self._verify_completion(spec)

def _refine_remaining_steps(
    self,
    steps: list[StepSpec],
    knowledge: list[dict],
    kg_view: dict
) -> list[StepSpec]:
    """Refine steps based on accumulated knowledge."""

    prompt = f"""
    Original remaining steps: {steps}

    New knowledge from execution:
    - Files discovered: {knowledge[-1].get('new_files', [])}
    - APIs found: {knowledge[-1].get('apis', [])}
    - Current codebase structure: {kg_view}

    Refine these steps to incorporate this knowledge.
    Remove steps that are no longer needed.
    Add steps for newly discovered requirements.
    """

    refined = self.llm.refine_steps(prompt)
    return refined
```

**Expected Impact**: +2-4pp
- Dynamic adaptation vs rigid plan
- Remove unnecessary steps (discovered something exists)
- Add needed steps (discovered new requirements)

---

### Phase 4: Memory-Informed Self-Correction

**Why better than Factory Droid**:
Factory Droid: Send error to LLM, hope for the best
Adaptive: Query memory for proven solutions

```python
def _memory_informed_correction(
    self,
    step: StepSpec,
    result: StepResult
) -> StepSpec:
    """Correct step using memory of past solutions."""

    error_text = result.error

    # Query memory: Have we seen this error before?
    similar_errors = self.episodic.query_similar_errors(error_text, limit=5)

    if similar_errors:
        # Use proven solution from past
        best_match = similar_errors[0]
        if best_match["solution_worked"]:
            self._logger.info(f"Found matching error, applying known fix")
            return self._apply_known_fix(step, best_match["solution"])

    # Query codebase: Find similar implementations
    similar_code = self.vector.search(
        f"implementation of {step.goal}",
        top_k=5
    )

    # Generate correction with full context
    correction_prompt = f"""
    Step failed: {step}
    Error: {error_text}

    Similar past errors and solutions: {similar_errors[:3]}
    Similar code in codebase: {similar_code[:3]}

    Generate corrected step.
    """

    corrected = self.llm.correct_step(correction_prompt)
    return corrected
```

**Expected Impact**: +2-3pp
- Don't repeat known mistakes
- Use proven solutions
- Faster correction (no guessing)

---

### Phase 5: Confidence-Based Execution Depth

**Why better than Factory Droid**:
Not all steps need the same validation depth.

```python
def _classify_step_complexity(self, step: StepSpec) -> str:
    """Classify step for execution depth."""

    if step.type in ["mkdir", "cd", "echo", "cat"]:
        return "simple"  # Light validation

    elif step.type in ["write_file", "edit_file"]:
        if step.language in ["python", "javascript", "typescript"]:
            return "code"  # LSP validation
        return "file"  # Basic validation

    elif step.type in ["delete", "modify_config", "run_migration"]:
        return "critical"  # Full validation + rollback plan

    return "standard"

def _execute_by_complexity(self, step: StepSpec) -> StepResult:
    """Execute with appropriate validation depth."""

    complexity = self._classify_step_complexity(step)

    if complexity == "simple":
        # Direct execution, light validation
        return self._execute_fast(step)

    elif complexity == "code":
        # LSP pre-validation
        return self._execute_step_with_lsp(step)

    elif complexity == "critical":
        # Full validation + rollback
        rollback = self._create_rollback_plan(step)
        result = self._execute_with_full_validation(step)
        if not result.success:
            self._apply_rollback(rollback)
        return result

    return self._execute_standard(step)
```

**Expected Impact**: +1-2pp
- Faster simple steps
- More thorough critical steps
- Right validation for each step

---

## Implementation Plan

### Phase 1: Memory-Informed Spec Generation (6-8h)
1. Add memory query methods to spec generator
2. Create similar task query in DuckDB
3. Integrate failure pattern lookup
4. Test with 3 tasks (simple, medium, complex)

### Phase 2: LSP-Integrated Execution (4-6h)
1. Integrate LSP validation into execution loop
2. Add code block extraction from commands
3. Apply auto-fixes before execution
4. Measure retry reduction

### Phase 3: Adaptive Refinement Loop (4-6h)
1. Add knowledge extraction from results
2. Implement step refinement logic
3. Update Knowledge Graph after file creation
4. Test adaptation on multi-step tasks

### Phase 4: Memory-Informed Correction (3-4h)
1. Add error pattern storage to DuckDB
2. Implement similar error query
3. Track solution success rates
4. Test on known failure patterns

### Phase 5: Confidence-Based Execution (2-3h)
1. Implement step classifier
2. Add execution depth routing
3. Add rollback for critical steps
4. Validate with mixed-complexity tasks

**Total**: 19-27 hours
**Expected Accuracy**: 62-68% (vs Factory Droid's 58.8%)

---

## Comparison Summary

| Aspect | Factory Droid | Adaptive Spec |
|--------|---------------|---------------|
| Spec Generation | Blind | Memory-informed |
| Code Validation | Post-execution | Pre-execution (LSP) |
| Self-Correction | Error-only | Memory + context |
| Spec Evolution | Static | Adaptive |
| Execution Depth | Uniform | Confidence-based |
| Learning | None | Cross-session |

---

## Success Metrics

**Primary**: Terminal-Bench accuracy >60%
**Secondary**:
- Retry loops reduced >30% (LSP pre-validation)
- Spec refinements per task (measure adaptation)
- Memory hits per task (measure reuse)
- Self-correction success rate >80%

---

## References

- Factory Droid analysis: ai/research/factory-droid-architecture.md
- Memory architecture: ai/research/memory-system-architecture.md
- LSP integration: src/aircher/tools/lsp_autofix.py
- Knowledge Graph: src/aircher/memory/knowledge_graph.py
- Episodic memory: src/aircher/memory/duckdb_memory.py
