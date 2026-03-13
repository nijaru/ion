# Autoresearch and RLM Patterns

*Date: 2026-03-13*

## Context

We reviewed Andrej Karpathy's [`autoresearch`](https://github.com/karpathy/autoresearch) project to extract patterns and learnings applicable to our own `ion` agent runtime, specifically around Subagents, Swarms, and RLM (Reinforcement Learning from Language Models) loops.

`autoresearch` is an experiment in fully autonomous AI research where agents run overnight, modifying code, training models, evaluating metrics, and deciding whether to keep or discard changes.

## Core Patterns

### 1. Heavily Constrained RLM Loops
The system is successful because it is rigorously constrained:
- **Single File Edit:** The agent only modifies `train.py`. The data prep and utilities (`prepare.py`) are strictly off-limits.
- **Fixed Time Budget:** Every experiment runs for exactly 5 minutes (wall clock), regardless of the architecture.
- **Single Objective Metric:** The success of an edit is determined by one scale-independent metric (`val_bpb` - validation bits per byte). Lower is better. 

**Takeaway for ion:** When we build autonomous loops, they need strict, empirical halting conditions. Open-ended "make it better" loops fail. We need a fast, objective feedback mechanism (like a test suite or a compiler) to evaluate each subagent's changes.

### 2. "Programmatic" Prompting via Markdown
Instead of the human writing code, the human writes the *system*.
- **`program.md`:** This file acts as the baseline instructions and context for the agent. The human iterates on `program.md` to find the "research org code" that produces the best results, rather than iterating on `train.py` directly.

**Takeaway for ion:** This perfectly validates our `ai/` directory structure (`STATUS.md`, `DESIGN.md`). The files themselves are the programming interface for the agents.

### 3. The Local Optima Problem & Guidance Agents
One of the most valuable community insights from `autoresearch` (specifically Issue #179) is the problem of long-running execution agents getting stuck in local optima.

- **The Problem:** An execution agent might successfully optimize a narrow line of thought (e.g., tweaking batch sizes) but fail to step back and try an entirely different architecture because its context window is filled with the immediate history of its recent tweaks. It loses the forest for the trees.
- **The Solution (Guidance Agent):** A proposed pattern is splitting the swarm into two roles:
  1. **Guidance Agent:** Responsible purely for reading, synthesizing, judging, and maintaining a "project-level long-term memory file." It filters noise and distills reusable insights.
  2. **Execution Agent:** The agent actually writing the code and running the loops. It uses the memory file maintained by the Guidance Agent to align its efforts.

**Takeaway for ion:** As we design subagents and swarms (`tk-npsw`), we should avoid generic identical agents. Role specialization is key. We already do this manually (e.g., using `researcher` vs `developer`), but a native runtime swarm could have a persistent "Guidance" subagent running asynchronously, continually updating a `DESIGN.md` or `STATUS.md` file while the "Execution" agents grind on the code.

## Conclusion for Native Ion Runtime

1. The `AgentSession` interface we built must support streaming events from *multiple* agents simultaneously to render a swarm correctly.
2. We should formalize the concept of "Objective Functions" (like tests or compile checks) as first-class citizens in our RLM loops.
3. The "Guidance Agent" pattern is a strong architectural blueprint for how to keep autonomous swarms from degrading into local optima loops.
