# Subagent Architecture

**Purpose**: Keep main context clean while enabling research and exploration

## Philosophy

Subagents are for **context isolation**, not complex orchestration:

- Main thread with main model handles complex reasoning
- Subagents handle research/exploration that would pollute context
- Results returned as formatted summaries, not raw data

## Built-in Subagents

Only two built-in subagents needed:

| Subagent   | Purpose                    | When to Use                            |
| ---------- | -------------------------- | -------------------------------------- |
| `research` | Web search, format results | External docs, current info            |
| `explorer` | Codebase navigation        | Finding files, understanding structure |

Complex work stays on main thread.

## Web Search Tool

Built-in like Claude Code's `web_search` and `web_fetch`. **Subagent formats results** for main agent context efficiency.

### Search Provider Options

No paid APIs built into aircher. User configures their preferred provider:

| Provider     | Type      | Config                     |
| ------------ | --------- | -------------------------- |
| DuckDuckGo   | Free      | Default, no API key needed |
| Brave Search | Free tier | `BRAVE_API_KEY`            |
| Exa          | Paid      | `EXA_API_KEY`              |
| Tavily       | Paid      | `TAVILY_API_KEY`           |
| SerpAPI      | Paid      | `SERPAPI_KEY`              |

```typescript
// User configures in .aircher/config.json
interface SearchConfig {
  provider: "duckduckgo" | "brave" | "exa" | "tavily" | "serpapi";
  apiKey?: string; // Required for paid providers
}

// Default: DuckDuckGo (free, no key required)
const DEFAULT_SEARCH_PROVIDER = "duckduckgo";
```

### How It Works

```typescript
// Main agent calls web_search tool
// Tool spawns research subagent with separate context
// Subagent: searches → fetches → formats → returns summary only

async function webSearch(query: string): Promise<WebSearchResult> {
  // 1. Search via configured provider
  const rawResults = await searchProvider.search(query);

  // 2. Fetch top results
  const pages = await fetchPages(rawResults.slice(0, 5));

  // 3. Research subagent formats for main agent
  // Key: subagent has its own context, main agent only sees summary
  const formatted = await researchSubagent.run(`
    Extract relevant information for: "${query}"

    Return only:
    - Key facts answering the query
    - Code examples if relevant
    - Source URLs for citation
  `);

  return formatted; // Summary only, not full page content
}
```

**Value**: Main agent context stays clean. Even with same model cost, keeping full web page content out of main context improves reasoning quality.

## Context7 Docs Tool

Built-in library documentation via Context7. Provides LLM-optimized docs.

```typescript
interface DocsResult {
  library: string;
  topic?: string;
  content: string; // Already LLM-formatted by Context7
  codeExamples: string[];
}

async function getDocs(library: string, topic?: string): Promise<DocsResult> {
  // 1. Resolve library ID
  const libraryId = await context7.resolveLibraryId(library);

  // 2. Fetch docs (already optimized)
  const docs = await context7.getLibraryDocs({
    context7CompatibleLibraryID: libraryId,
    topic,
    mode: "code", // API refs and examples
  });

  return {
    library,
    topic,
    content: docs.content,
    codeExamples: docs.examples,
  };
}
```

Usage by agent:

```
Agent: I need to understand how to use libsql in Bun
→ getDocs("libsql", "bun integration")
→ Returns formatted docs with code examples
```

## LSP Integration

Like OpenCode - agent can query LSP for:

- Diagnostics (errors, warnings)
- Type information
- Go to definition
- Find references
- Completions

```typescript
interface LSPTools {
  getDiagnostics(file: string): Diagnostic[];
  getTypeAtPosition(file: string, line: number, col: number): TypeInfo;
  getDefinition(file: string, line: number, col: number): Location;
  getReferences(file: string, line: number, col: number): Location[];
}

// Agent uses LSP tools directly (no subagent needed)
// These are fast, synchronous operations
```

Benefits:

- Agent sees same errors as IDE
- Can validate code before saving
- Understands types without reading all files

## Explorer Subagent

Inspired by Amp's "look at" pattern: analyze with a **goal in mind**, return only relevant info.

### When to Trigger

| Trigger                       | Example                                                     |
| ----------------------------- | ----------------------------------------------------------- |
| Open-ended codebase questions | "How does auth work in this project?"                       |
| Finding relevant files        | "Find files related to database migrations"                 |
| Understanding structure       | "What's the architecture of the API layer?"                 |
| Large file analysis           | "Look at this 2000-line file and find the validation logic" |

**NOT for**: Simple file reads, specific line lookups, known paths.

### How It Works

```typescript
interface ExploreRequest {
  goal: string; // What to find/understand
  scope?: string; // Directory, file pattern, or "codebase"
  maxFiles?: number; // Limit exploration breadth
}

interface ExploreResult {
  summary: string; // High-level answer to goal
  relevantFiles: string[]; // Paths main agent might need
  keyFindings: string[]; // Specific facts extracted
  codeSnippets?: string[]; // Small relevant snippets only
}

// Explorer has its own context - main agent never sees full file contents
async function explore(request: ExploreRequest): Promise<ExploreResult> {
  // 1. Subagent searches codebase
  const files = await glob(request.scope || "**/*");
  const matches = await grep(request.goal, files);

  // 2. Subagent reads and analyzes (in its own context)
  const analysis = await explorerSubagent.run(`
    Goal: ${request.goal}
    Files found: ${matches.join(", ")}

    Read relevant files and extract:
    - Direct answer to the goal
    - Key code patterns/structures
    - File paths main agent should look at

    DO NOT return full file contents.
  `);

  // 3. Return summary only to main agent
  return analysis;
}
```

### Value: Context Isolation

Even with same model cost, exploration value is **keeping main context clean**:

```
Without explorer:
  Main agent reads 10 files → 50k tokens in context → degraded reasoning

With explorer:
  Explorer reads 10 files in separate context
  Returns 500 token summary to main agent
  Main context stays focused
```

This matches Amp's "look at" - the main agent gets distilled information, never processes full files unless explicitly needed.

## Model Selection

```typescript
const MODELS = {
  // Default: same price as V3.2, optimized for agentic use
  default: "deepseek/deepseek-v3.2-speciale",

  // Standard variant (if Speciale unavailable)
  standard: "deepseek/deepseek-v3.2",

  // Anthropic (quality tier)
  opus: "anthropic/claude-opus-4.5",
  sonnet: "anthropic/claude-sonnet-4.5",
  haiku: "anthropic/claude-haiku-4.5",

  // OpenAI
  codex: "openai/gpt-5.1-codex-max",
  gpt52: "openai/gpt-5.2",

  // Google
  gemini: "google/gemini-3-pro-preview",

  // Chinese (affordable alternatives)
  glm: "z-ai/glm-4.6",
};

// Default: DeepSeek V3.2 for cost efficiency
// Escalate to Speciale or Opus for complex reasoning
```

## Implementation

### Phase 8.3: Core Tools

```typescript
// Built-in tools (not subagents)
const CORE_TOOLS = [
  "read_file",
  "write_file",
  "edit_file",
  "bash",
  "glob",
  "grep",
  "web_search", // Uses research subagent internally
  "web_fetch", // Direct fetch + format
  "get_docs", // Context7 integration
  "lsp_diagnostics",
  "lsp_type_info",
];
```

### Phase 8.4: Subagent Infrastructure

```typescript
class SubagentSession {
  private context: Message[] = [];

  async run(prompt: string): Promise<string> {
    // Isolated context
    // Limited tools (no recursion)
    // Returns text summary only
  }
}

// Only two built-in subagent types
const SUBAGENTS = {
  research: {
    model: MODELS.affordable,
    tools: ["web_search", "web_fetch"],
    systemPrompt: "Format search results for LLM consumption...",
  },
  explorer: {
    model: MODELS.affordable,
    tools: ["read_file", "glob", "grep"],
    systemPrompt: "Explore codebase and summarize findings...",
  },
};
```

## Key Principles

1. **Main thread for complex work** - Subagents have reduced effectiveness on complex reasoning
2. **Subagents for isolation** - Keep research/exploration out of main context
3. **Formatted results** - Never return raw HTML/data to main agent
4. **Built-in tools over subagents** - LSP, Context7 are direct tools, not subagents
5. **DeepSeek default** - Most affordable, escalate when needed

## References

- Claude Code web_search/web_fetch pattern
- Context7 library docs API
- OpenCode LSP integration
