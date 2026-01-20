# Archetype Pattern Specification

## Overview

Archetypes customize the memory framework for specific domains. The framework provides storage and retrieval primitives; archetypes provide domain semantics.

## Design Principles

1. **Separation of concerns** - Framework stable, archetypes evolve
2. **Domain language** - Users think in domain terms
3. **Pluggable** - Easy to add new archetypes
4. **Configurable** - Override defaults per use case

## Archetype Interface

```typescript
interface Archetype<TEventTypes extends string = string> {
  /** Unique name for this archetype */
  name: string;

  /** Human-readable description */
  description: string;

  /** Version for compatibility */
  version: string;

  // Domain-specific type definitions
  eventTypes: readonly TEventTypes[];
  entityTypes: readonly string[];
  relationTypes: readonly string[];
  patternTypes: readonly string[];

  // Extractors - how to derive knowledge from events
  extractors: EventExtractors<TEventTypes>;
  relationExtractors: RelationExtractors<TEventTypes>;

  // Context strategy - how to assemble prompts
  contextStrategy: ContextStrategy;

  // Tools - domain-specific tools
  tools: ToolDefinition[];

  // Hooks - lifecycle customization
  hooks?: ArchetypeHooks;
}
```

## Extractors

### Entity Extractors

Extract entities from events:

```typescript
type EventExtractors<T extends string> = {
  [K in T]?: (event: Event) => ExtractedEntity[];
};

interface ExtractedEntity {
  type: string;
  name: string;
  aliases?: string[];
  properties?: Record<string, unknown>;
  salience?: number;
}
```

### Relation Extractors

Extract relations between entities:

```typescript
type RelationExtractors<T extends string> = {
  [K in T]?: (event: Event, entities: Entity[]) => ExtractedRelation[];
};

interface ExtractedRelation {
  type: string;
  sourceId: string;
  targetId: string;
  weight?: number;
  metadata?: Record<string, unknown>;
}
```

## Context Strategy

How to assemble context for prompts:

```typescript
interface ContextStrategy {
  /** Weight for recent events (0-1) */
  recencyWeight: number;

  /** Weight for semantic similarity (0-1) */
  relevanceWeight: number;

  /** Weight for important entities (0-1) */
  salientWeight: number;

  /** How deep to traverse relations */
  depthLimit: number;

  /** Include failed events? */
  includeFailures: boolean;

  /** Token budget allocation */
  budgetAllocation: {
    recent: number; // % for recent events
    semantic: number; // % for semantic search
    entities: number; // % for entity context
    relations: number; // % for related context
  };

  /** Custom context assembly */
  customAssembly?: (results: MemoryResults) => Context;
}
```

## Hooks

Lifecycle customization:

```typescript
interface ArchetypeHooks {
  /** Before recording an event */
  beforeRecord?: (event: Event) => Event | null;

  /** After recording an event */
  afterRecord?: (event: Event) => void;

  /** Before context assembly */
  beforeContextAssembly?: (request: ContextRequest) => ContextRequest;

  /** After context assembly */
  afterContextAssembly?: (context: Context) => Context;

  /** On session start */
  onSessionStart?: (sessionId: string) => void;

  /** On session end */
  onSessionEnd?: (sessionId: string) => void;
}
```

## Built-in Archetypes

### Coding Archetype

For AI coding agents:

```typescript
const codingArchetype: Archetype = {
  name: "coding",
  description: "AI coding agent for software development",
  version: "1.0.0",

  eventTypes: [
    "file_read",
    "file_write",
    "file_edit",
    "shell_exec",
    "search",
    "error",
    "user_message",
    "agent_response",
  ],

  entityTypes: [
    "file",
    "function",
    "class",
    "module",
    "variable",
    "error_pattern",
    "dependency",
  ],

  relationTypes: [
    "imports",
    "exports",
    "defines",
    "calls",
    "extends",
    "implements",
    "depends_on",
    "caused_by",
    "related_to",
  ],

  patternTypes: [
    "error_fix",
    "refactor",
    "test_pattern",
    "optimization",
    "security_fix",
  ],

  extractors: {
    file_read: (event) => {
      const path = event.content.path;
      return [
        {
          type: "file",
          name: path,
          properties: {
            language: detectLanguage(path),
            lastRead: event.timestamp,
          },
          salience: 0.5,
        },
      ];
    },

    file_write: (event) => {
      const path = event.content.path;
      return [
        {
          type: "file",
          name: path,
          properties: {
            language: detectLanguage(path),
            lastWrite: event.timestamp,
            linesChanged: event.content.diff?.split("\n").length,
          },
          salience: 0.7, // Writes are more salient
        },
      ];
    },

    shell_exec: (event) => {
      const entities: ExtractedEntity[] = [];

      // Extract command as entity
      const command = event.content.command.split(" ")[0];
      entities.push({
        type: "tool",
        name: command,
        properties: { fullCommand: event.content.command },
        salience: 0.3,
      });

      // Extract error patterns
      if (event.outcome === "failure" && event.content.stderr) {
        entities.push({
          type: "error_pattern",
          name: extractErrorType(event.content.stderr),
          properties: {
            stderr: event.content.stderr,
            command: event.content.command,
          },
          salience: 0.8,
        });
      }

      return entities;
    },

    error: (event) => [
      {
        type: "error_pattern",
        name: event.content.type || "unknown_error",
        properties: {
          message: event.content.message,
          stack: event.content.stack,
          file: event.content.file,
          line: event.content.line,
        },
        salience: 0.9, // Errors are highly salient
      },
    ],
  },

  relationExtractors: {
    file_read: async (event, entities) => {
      const relations: ExtractedRelation[] = [];
      const content = event.content.text;

      if (content) {
        const imports = parseImports(content);
        const fileEntity = entities.find((e) => e.type === "file");

        if (fileEntity) {
          for (const imp of imports) {
            relations.push({
              type: "imports",
              sourceId: fileEntity.id,
              targetId: resolveImportPath(imp, event.content.path),
              metadata: { importStatement: imp },
            });
          }
        }
      }

      return relations;
    },

    error: (event, entities) => {
      const relations: ExtractedRelation[] = [];
      const errorEntity = entities.find((e) => e.type === "error_pattern");

      if (errorEntity && event.content.file) {
        relations.push({
          type: "caused_by",
          sourceId: errorEntity.id,
          targetId: event.content.file,
          weight: 0.9,
        });
      }

      return relations;
    },
  },

  contextStrategy: {
    recencyWeight: 0.3,
    relevanceWeight: 0.5,
    salientWeight: 0.2,
    depthLimit: 2,
    includeFailures: true, // Errors are valuable context

    budgetAllocation: {
      recent: 20,
      semantic: 40,
      entities: 20,
      relations: 20,
    },
  },

  tools: [
    { name: "read_file", description: "Read file contents" },
    { name: "write_file", description: "Write file contents" },
    { name: "edit_file", description: "Edit file with diff" },
    { name: "bash", description: "Execute shell command" },
    { name: "glob", description: "Find files by pattern" },
    { name: "grep", description: "Search file contents" },
  ],

  hooks: {
    afterRecord: (event) => {
      // Auto-learn patterns from successful error fixes
      if (event.type === "shell_exec" && event.outcome === "success") {
        // Check if this fixed a recent error
        // If so, record as pattern
      }
    },
  },
};
```

### Support Archetype

For customer support agents:

```typescript
const supportArchetype: Archetype = {
  name: "support",
  description: "Customer support agent",
  version: "1.0.0",

  eventTypes: [
    "message",
    "ticket_created",
    "ticket_updated",
    "ticket_resolved",
    "escalation",
    "knowledge_search",
  ],

  entityTypes: [
    "customer",
    "ticket",
    "product",
    "issue_category",
    "resolution",
  ],

  relationTypes: [
    "reported_by",
    "assigned_to",
    "resolved_by",
    "related_to",
    "escalated_to",
  ],

  patternTypes: ["resolution_path", "escalation_trigger", "faq_answer"],

  extractors: {
    message: (event) => {
      const entities: ExtractedEntity[] = [];

      if (event.content.customerId) {
        entities.push({
          type: "customer",
          name: event.content.customerId,
          properties: {
            email: event.content.email,
            name: event.content.name,
          },
          salience: 0.7,
        });
      }

      // Extract mentioned products
      const products = extractProductMentions(event.content.text);
      for (const product of products) {
        entities.push({
          type: "product",
          name: product,
          salience: 0.5,
        });
      }

      return entities;
    },

    ticket_created: (event) => [
      {
        type: "ticket",
        name: event.content.ticketId,
        properties: {
          subject: event.content.subject,
          priority: event.content.priority,
          category: event.content.category,
        },
        salience: 0.9,
      },
    ],
  },

  relationExtractors: {
    ticket_created: (event, entities) => {
      const relations: ExtractedRelation[] = [];
      const ticket = entities.find((e) => e.type === "ticket");
      const customer = entities.find((e) => e.type === "customer");

      if (ticket && customer) {
        relations.push({
          type: "reported_by",
          sourceId: ticket.id,
          targetId: customer.id,
        });
      }

      return relations;
    },
  },

  contextStrategy: {
    recencyWeight: 0.4, // Recent conversation matters
    relevanceWeight: 0.4,
    salientWeight: 0.2,
    depthLimit: 1,
    includeFailures: false, // Don't remind of failures

    budgetAllocation: {
      recent: 30,
      semantic: 35,
      entities: 20,
      relations: 15,
    },
  },

  tools: [
    { name: "search_knowledge", description: "Search knowledge base" },
    { name: "get_customer", description: "Get customer info" },
    { name: "update_ticket", description: "Update ticket" },
    { name: "escalate", description: "Escalate to human" },
  ],
};
```

### Research Archetype

For research/analysis agents:

```typescript
const researchArchetype: Archetype = {
  name: "research",
  description: "Research and analysis agent",
  version: "1.0.0",

  eventTypes: ["search", "read_source", "extract_fact", "synthesize", "cite"],

  entityTypes: ["source", "author", "claim", "topic", "citation"],

  relationTypes: [
    "cites",
    "supports",
    "contradicts",
    "authored_by",
    "related_to",
    "part_of",
  ],

  patternTypes: [
    "search_strategy",
    "synthesis_approach",
    "verification_method",
  ],

  contextStrategy: {
    recencyWeight: 0.2,
    relevanceWeight: 0.6, // Relevance matters most
    salientWeight: 0.2,
    depthLimit: 3, // Deep relation traversal
    includeFailures: true,

    budgetAllocation: {
      recent: 10,
      semantic: 50,
      entities: 20,
      relations: 20,
    },
  },

  // ... extractors, tools
};
```

## Creating Custom Archetypes

### Minimal Archetype

```typescript
import { Archetype, createArchetype } from "aircher";

const myArchetype = createArchetype({
  name: "my-agent",
  description: "My custom agent",

  eventTypes: ["action", "observation"],
  entityTypes: ["item"],
  relationTypes: ["related_to"],
  patternTypes: ["workflow"],

  extractors: {
    action: (event) => [
      {
        type: "item",
        name: event.content.target,
      },
    ],
  },

  contextStrategy: {
    recencyWeight: 0.5,
    relevanceWeight: 0.5,
    salientWeight: 0.0,
    depthLimit: 1,
    includeFailures: false,
    budgetAllocation: { recent: 50, semantic: 50, entities: 0, relations: 0 },
  },

  tools: [],
});
```

### Extending Existing Archetypes

```typescript
import { codingArchetype, extendArchetype } from "aircher";

const customCodingArchetype = extendArchetype(codingArchetype, {
  name: "custom-coding",

  // Add new event types
  eventTypes: [...codingArchetype.eventTypes, "deploy", "test_run"],

  // Add new extractors
  extractors: {
    ...codingArchetype.extractors,
    deploy: (event) => [
      {
        type: "deployment",
        name: event.content.environment,
        properties: { version: event.content.version },
      },
    ],
  },

  // Override context strategy
  contextStrategy: {
    ...codingArchetype.contextStrategy,
    recencyWeight: 0.4, // More weight on recent
  },
});
```

## Archetype Registration

```typescript
import { Memory, registerArchetype } from "aircher";

// Register custom archetype
registerArchetype(myArchetype);

// Use archetype
const memory = new Memory({
  archetype: "my-agent", // or pass archetype object
  projectPath: "/path/to/project",
});
```

## Best Practices

### Event Type Design

1. **Be specific** - `file_read` not `action`
2. **Include outcome** - Use `success`/`failure`
3. **Consistent schema** - Same content structure per type

### Entity Extraction

1. **Extract liberally** - Better to over-extract
2. **Set salience** - Indicate importance
3. **Add aliases** - Help matching

### Relation Extraction

1. **Be conservative** - Only confident relations
2. **Include evidence** - Link to source events
3. **Set weights** - Indicate confidence

### Context Strategy

1. **Domain-appropriate** - Support agents need recency, research needs relevance
2. **Test empirically** - Measure context quality
3. **Iterate** - Adjust based on agent performance

## References

- [memory-architecture.md](memory-architecture.md) - Storage layer
- [context-assembly.md](context-assembly.md) - How context is built
- [api-design.md](api-design.md) - Public API
