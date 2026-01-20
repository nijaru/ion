# Design Spec: Agent & Tools

## 1. Overview
The Agent system handles task execution through a multi-turn loop, orchestrating between the main agent, specialized sub-agents, and behavior-modifying skills.

## 2. Core Execution Loop
- **Multi-Turn**: Continues until the model produces no more tool calls or a terminal response.
- **Phases**:
  1. **Recall**: Retrieve long-term memory (OmenDB).
  2. **Stream**: Generate reasoning and tool calls.
  3. **Execute**: Parallel tool execution via `JoinSet`.
  4. **Compact**: Background context pruning if needed.
  5. **Learn**: Index outcome into OmenDB.

## 3. Specialization

### 3.1 Sub-Agents (Context Isolation)
Used for "expansion" tasks where context noise should be minimized.
- **Explorer**: Fast model, iterative search/glob.
- **Researcher**: Web search, documentation synthesis.
- **Reviewer**: Full validation (build/test/analyze).

### 3.2 Skills (Context Preservation)
Prompt-based behavior modifiers injected into the main conversation.
- **Developer**: Implementation-focused prompts.
- **Designer**: Architectural planning.
- **Refactor**: Large-scale restructuring.

## 4. Tool Orchestrator
- **Modes**:
  - `Read`: No permissions needed.
  - `Write`: Interactive prompts (`y/n/a/A`).
  - `Agi`: Autonomous execution (YOLO mode).
- **Parallelism**: `SupportsParallel` flag allows tools like `read` to run concurrently while `bash` remains serial.
- **Permissions**: Decision caching per session.

## 5. Implementation
- `src/agent/`: Main loop and sub-agent spawning.
- `src/skill/`: `SKILL.md` loading and injection.
- `src/tool/`: Registry, orchestration, and built-ins.
