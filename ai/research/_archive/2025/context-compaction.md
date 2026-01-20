# Context Compaction Strategy

**Purpose**: Enable infinite conversations through intelligent background compaction

## Problem Statement

Context limits (100k-200k tokens) force conversation restarts. Current solutions:

| Approach           | Issues                                     |
| ------------------ | ------------------------------------------ |
| Manual `/compact`  | User burden, interrupts flow               |
| Auto at 80%        | Full summary loses context (Claude Code)   |
| Full summarization | Loses debugging context, repeated mistakes |

**Goal**: Automatic, reliable compaction that preserves essential context.

## Industry Analysis

### Claude Code (Issues & Lessons)

**Trigger**: 80% of 200k tokens (~160k)
**Process**: LLM summary → new session with summary

**Known problems**:

- Infinite retry loops (#6004)
- Context corruption after failure (#3274)
- Loss of debugging info in summaries
- Claude repeats failed approaches post-compaction

**Root cause**: Full summarization loses too much context; summary quality varies by task type.

### Codex CLI (Better Pattern)

```
Trigger: 180k-244k tokens (model-dependent)
Strategy: Summary + preserve last 20k tokens
```

**Key insight**: Hybrid preservation (summary + recent verbatim) more robust than full summarization.

### OpenCode (Tiered Pruning)

```
Trigger: (context_limit - output_limit)
Strategy:
  1. Prune tool outputs first
  2. Protected 40k-token window for recent turns
  3. Full compaction as last resort
```

**Key insight**: Prune aggressively prunable content (tool outputs) before touching conversation.

### Google ADK (Tiered Architecture)

```
Working Context (per-call) ← curated from:
├── Session (event log)
├── Memory (cross-session)
└── Artifacts (large blobs, loaded on-demand)
```

**Key insight**: Separate storage from presentation. Session is permanent; context is curated view.

### Amp (Manual Only)

Philosophy: User should control compaction. Provides Handoff/Fork tools instead of auto-compaction.

**Key insight**: Power users prefer control. Auto-compaction should be optional.

## Aircher Strategy

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│ Stable Prefix (cacheable, rarely changes)               │
│ ├── System prompt                                       │
│ ├── Learned context (.aircher/learned/)                 │
│ └── Extracted facts (structured, from compaction)       │
├─────────────────────────────────────────────────────────┤
│ Compressed History (agent-curated)                      │
│ ├── High relevance → verbatim                           │
│ ├── Medium relevance → summarized                       │
│ ├── Low relevance → reference only                      │
│ └── Irrelevant → dropped                                │
├─────────────────────────────────────────────────────────┤
│ Variable Suffix (current turn)                          │
│ ├── Active tool outputs                                 │
│ └── On-demand artifact loads                            │
└─────────────────────────────────────────────────────────┘
```

### Agent-Driven Compression

No fixed turn limits. Agent scores each turn by relevance and compresses proportionally.

```typescript
type CompressionStrategy = "keep" | "summarize" | "reference" | "drop";

interface TurnAnalysis {
  turnId: string;
  strategy: CompressionStrategy;
  relevanceScore: number; // 0-1
  reason: string;
}

interface CompactionPlan {
  turns: TurnAnalysis[];
  extractedFacts: ExtractedFact[]; // Structured data to preserve
  estimatedTokens: number;
}

const COMPACTION_PROMPT = `Analyze conversation for compaction. Current task: {task}

For each turn, assign a strategy:
- KEEP: Critical for current task, keep verbatim
- SUMMARIZE: Important context, condense to key points
- REFERENCE: Keep file paths/commands only, drop output (can re-execute)
- DROP: No future value, safe to remove

Requirements:
1. NEVER drop failed approaches (prevents repeating mistakes)
2. Preserve all file paths and URLs (restorable)
3. Extract key facts/decisions into structured format
4. Optimize for information density
5. Recent turns need not be kept if irrelevant to current task
6. Old turns should be kept if still relevant

Output JSON:
{
  "turns": [{ "turnId": "...", "strategy": "...", "relevanceScore": 0.8, "reason": "..." }],
  "extractedFacts": [{ "type": "decision|error|file|dependency", "content": "...", "source": "turn-id" }],
  "estimatedTokens": 12000
}`;
```

### Structured Fact Extraction

Extract structured data during compaction for better retrieval:

```typescript
interface ExtractedFact {
  type: "decision" | "error" | "file_state" | "dependency" | "constraint";
  content: string;
  source: string; // Turn ID for provenance
  timestamp: number;
}

// Examples:
// { type: "decision", content: "Using libsql instead of DuckDB for Bun compatibility" }
// { type: "error", content: "npm install fails with ERESOLVE - peer dep conflict" }
// { type: "file_state", content: "src/auth.ts: Added JWT validation, 150 lines" }
// { type: "constraint", content: "Must support Node 18+ (no Bun-only APIs)" }
```

Facts persist in stable prefix, enabling:

- Fast retrieval without re-reading full history
- Cross-session learning (promote to .aircher/learned/)
- Structured queries ("what errors have we seen?")

### Incremental Summarization

Don't wait for threshold - summarize incrementally during idle time:

```typescript
class IncrementalCompactor {
  private pendingTurns: Turn[] = [];
  private idleTimeout: number | null = null;

  onTurnComplete(turn: Turn) {
    this.pendingTurns.push(turn);

    // Debounce: wait for idle before processing
    if (this.idleTimeout) clearTimeout(this.idleTimeout);
    this.idleTimeout = setTimeout(() => this.processIdle(), 5000);
  }

  private async processIdle() {
    if (this.pendingTurns.length < 3) return; // Wait for batch

    // Incrementally compress older pending turns
    const toProcess = this.pendingTurns.slice(0, -2); // Keep last 2 fresh
    const compressed = await this.compressTurns(toProcess);

    // Replace in context
    this.context.replaceTurns(toProcess, compressed);
    this.pendingTurns = this.pendingTurns.slice(-2);
  }
}
```

Benefits:

- Smoother token usage (no sudden drops)
- Better compression quality (smaller batches)
- Always ready for user input

### Tiered Pruning Pipeline

When approaching limit, execute tiers in order:

```typescript
async function pruneContext(context: Context): Promise<PruningResult> {
  const threshold = context.maxTokens * 0.85;
  if (context.tokens < threshold) return { strategy: "none" };

  // Tier 1: Truncate large tool outputs (keep head + tail)
  await context.truncateLargeOutputs({ maxPerOutput: 2000, keepLines: 50 });
  if (context.tokens < threshold) return { strategy: "truncate_outputs" };

  // Tier 2: Agent-driven compression (replaces fixed window)
  const plan = await getCompactionPlan(context);
  await context.applyCompactionPlan(plan);
  if (context.tokens < threshold) return { strategy: "agent_compression" };

  // Tier 3: Aggressive - drop all REFERENCE items, summarize everything
  await context.aggressiveCompact();
  return { strategy: "aggressive" };
}
```

### Compaction Triggers

| Trigger          | Threshold    | Action                     |
| ---------------- | ------------ | -------------------------- |
| Token count      | 85% capacity | Tiered pruning             |
| User idle        | 5 seconds    | Incremental summarization  |
| Large output     | >5k tokens   | Immediate truncation       |
| Session duration | 30 minutes   | Background fact extraction |

### Restorable Compression

Keep references, drop reproducible content:

```typescript
interface CompressedTurn {
  original: {
    type: "file_read" | "command" | "search";
    target: string; // path, command, or query
  };
  summary: string;
  canRestore: boolean;
  restoreHint?: string; // "re-read file" | "re-run command"
}

// Example: 500-line file read → 1-line reference
// Before: { type: "file_read", path: "src/auth.ts", content: "..." } (2000 tokens)
// After: { type: "file_read", path: "src/auth.ts", summary: "JWT auth with refresh tokens", canRestore: true } (20 tokens)
```

### Memory Integration

Promote valuable extracted facts to long-term memory:

```typescript
async function promoteToMemory(facts: ExtractedFact[]) {
  for (const fact of facts) {
    if (fact.type === "decision" && isArchitectural(fact)) {
      // Persist to .aircher/learned/decisions.md
      await learnedContext.addDecision(fact);
    }
    if (fact.type === "error" && isRecurring(fact)) {
      // Persist to episodic memory for future sessions
      await episodicMemory.recordErrorPattern(fact);
    }
  }
}
```

### Background Processing

```typescript
class BackgroundCompactor {
  private queue: CompactionTask[] = [];
  private processing = false;

  async enqueue(task: CompactionTask) {
    this.queue.push(task);
    if (!this.processing) this.processQueue();
  }

  private async processQueue() {
    this.processing = true;
    while (this.queue.length > 0) {
      const task = this.queue.shift()!;

      // Snapshot before, restore on failure
      const snapshot = this.context.snapshot();
      try {
        await this.executeTask(task);
      } catch (error) {
        this.context.restore(snapshot);
        this.notify("Compaction failed, restored previous state");
      }

      // Yield to main thread
      await new Promise((r) => setTimeout(r, 0));
    }
    this.processing = false;
  }
}
```

### Configuration

```typescript
interface CompactionConfig {
  enabled: boolean; // Default: true
  triggerThreshold: number; // Default: 0.85
  incrementalEnabled: boolean; // Default: true
  idleDelayMs: number; // Default: 5000
  maxOutputTokens: number; // Default: 5000
  extractFacts: boolean; // Default: true
  promoteToMemory: boolean; // Default: true
  notifyOnCompact: boolean; // Default: false (silent by default)
}
```

### Quality Metrics

| Metric                    | Target | Measurement                                    |
| ------------------------- | ------ | ---------------------------------------------- |
| Compression ratio         | 3-5x   | tokens_before / tokens_after                   |
| Information retention     | >90%   | Can agent answer questions about dropped turns |
| Failed approach retention | 100%   | Never drop failed approaches                   |
| Restore success rate      | >95%   | Reference items successfully re-fetched        |
| Task continuation rate    | >99%   | Tasks complete successfully after compaction   |

## Implementation Phases

### Phase 1: Basic Infrastructure

- Token counting and threshold monitoring
- `/compact` command with agent-driven analysis
- Snapshot/restore for failure recovery

### Phase 2: Tiered Pruning

- Large output truncation (Tier 1)
- Agent-driven compression prompt (Tier 2)
- Aggressive fallback (Tier 3)

### Phase 3: Incremental Processing

- Idle-time summarization
- Debounced background processing
- Smoother token curves

### Phase 4: Structured Extraction

- Fact extraction during compaction
- Memory promotion (decisions → .aircher/learned/)
- Error pattern recording

### Phase 5: Optimization

- Context caching for stable prefixes
- Parallel compaction for large contexts
- Quality metric tracking

## References

- `research/context-engineering-concepts.md` - Session vs context theory
- `research/memory-system-architecture.md` - Memory layer integration
- `design/agent-architecture-sota.md` - SOTA patterns
- Claude Code issues: #6004, #3274, #6689, #9029
- Google ADK whitepaper: Context Engineering for AI Agents
- Codex CLI: Token threshold compaction
