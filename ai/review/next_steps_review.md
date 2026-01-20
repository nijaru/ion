I will begin by reviewing the project's current status and roadmap to understand the context of the open tasks.
I will examine the current implementation of prompts and error handling in the codebase to better assess the impact of the proposed tasks.
I will read `src/agent/mod.rs` to understand the current agent loop and how it handles task execution.
I will read `src/agent/context.rs` to understand the current context assembly logic and how `minijinja` could improve it.
I will read `ai/DESIGN.md` to see if there are any architectural plans or preferences that would favor one task over the others.
I will read `ai/review/next_steps_review.md` to see if it contains any relevant information from previous sessions that could influence the priority of the tasks.
Based on the current state of the **ion** project and the goals of maximizing reliability and developer experience (DX), the next priority should be:

### **Priority: `tk-4tfq` (Plan-Act-Verify loop with dynamic plan updates)**

**Rationale:**
The **Plan-Act-Verify** loop is the single most critical architectural upgrade for **agent reliability**. By implementing a self-correcting cycle where the agent must explicitly verify tool outputs against its original intentions and dynamically adjust its plan when reality deviates from expectations, you solve the "hallucination-of-success" problem (e.g., the agent assuming a bash command worked because the shell didn't crash, despite an error message in the output). This significantly boosts **developer experience** by reducing the need for manual oversight and providing a clear, observable state of the agent's reasoning.

While **`tk-ouxo` (minijinja)** is a vital infrastructural improvement for DX (decoupling instructions from Rust code), it is ultimately a "force multiplier" for the prompts used in the **Plan-Act-Verify** loop. Implementing the loop first (or in tandem) addresses the core functional requirement of a production-ready autonomous agent. **`tk-00sl`** (errors) and **`tk-ty77`** (TUI) are valuable polish tasks but do not offer the same leap in functional autonomy.
