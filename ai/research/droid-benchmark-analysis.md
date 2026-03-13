# Droid Architecture & Benchmark Research

*Date: 2026-03-13*

## Overview
[Droid](https://factory.ai/news/terminal-bench) is a high-performance, model-agnostic coding agent that consistently performs well on benchmarks like Terminal-Bench 2.0.

## Architecture & Reasoning Loops
- **Three-Tier Prompting:** Uses tool descriptions, system prompts, and mid-run "system notifications" to curb recency bias in long-running sessions.
- **Model-Specific Scaffolding:** While the core is model-agnostic, Droid uses model-specific adapters to optimize tool schemas and behavioral differences per provider.
- **Inline Planning Tool:** The agent uses a tool to create and update a concise plan. As it finishes steps, it crosses them off and marks the next step in progress, keeping the plan summary at the tail of the context window.
- **Controlled Background Execution:** Includes primitives for starting services (servers, etc.) that outlive the main agent process, allowing for long-running tests or dev environments.

## Context & Performance
- **Environmental Bootstrapping:** Frontloads salient system information (env vars, processes, etc.) as shell command output at the start of each session. This avoids redundant exploration turns.
- **Time-Aware Decision Making:** Annotates tool and session runtimes to bias the agent toward faster alternatives and smarter timeouts.

## Best-in-Class Takeaways for Ion
1. **Mid-Run System Notifications:** Inject time-sensitive context (e.g., current file state or recent errors) mid-trajectory to keep the agent aligned.
2. **Inline Plan Summary:** Keeping the current plan at the end of the context window is more effective for coherence than a separate hidden state.
3. **Environment Facts as Shell Output:** Presenting project context as "previous command output" (e.g., `cat AGENTS.md`) is often more reliable than a massive system prompt block.
4. **Tool Runtime Awareness:** Let the agent know how long tools take (e.g., "this grep took 2s") so it can adjust its strategy.
