# ion Plugin Architecture

## Overview

ion implements a Claude Code-compatible plugin system with hooks for lifecycle events and MCP for tool extensions.

## Design Goals

1. **Claude Code compatibility** - Largest plugin ecosystem
2. **Memory plugin support** - OmenDB memory plugin works out-of-box
3. **Simplicity** - Shell command hooks (no complex FFI)
4. **Separation** - Core agent vs plugins (memory as first plugin)

## Hook Events

| Event                | Timing                | Use Case                  |
| -------------------- | --------------------- | ------------------------- |
| `SessionStart`       | Session begins        | Load workspace context    |
| `SessionEnd`         | Session ends          | Cleanup, persist state    |
| `UserPromptSubmit`   | Before LLM call       | **Inject memory context** |
| `PreToolUse`         | Before tool execution | Security checks, logging  |
| `PostToolUse`        | After tool execution  | **Track changes**, format |
| `PostToolUseFailure` | Tool fails            | Error handling            |
| `PreCompact`         | Before compaction     | **Save working memory**   |
| `Stop`               | Agent stops           | Cleanup                   |
| `Notification`       | System notification   | Alerts                    |

Memory-critical hooks marked in **bold**.

## Hook Protocol

### Input (stdin JSON)

```json
{
  "event": "UserPromptSubmit",
  "session_id": "uuid",
  "cwd": "/path/to/project",
  "prompt": "user's prompt text",
  "tool_name": "Write",
  "tool_input": {},
  "tool_output": "result"
}
```

### Output (stdout)

- **Exit 0**: Success, stdout added to context
- **Exit 1**: Error, logged but continues
- **Exit 2**: Block operation with error message

### Timeout

- Default: 5000ms
- Configurable per-hook
- SIGTERM then SIGKILL after grace period

## Plugin Structure

```
my-plugin/
├── .claude-plugin/
│   └── plugin.json      # Plugin manifest
├── hooks/
│   ├── hooks.json       # Hook configuration (optional)
│   ├── inject-memory.sh # UserPromptSubmit hook
│   └── track-changes.sh # PostToolUse hook
├── mcp/
│   └── mcp.json         # MCP server config
├── commands/
│   └── remember.sh      # /remember command
└── skills/
    └── memory.md        # Memory skill instructions
```

### plugin.json

```json
{
  "name": "omendb-memory",
  "version": "0.1.0",
  "description": "Persistent memory for ion",
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "type": "command",
            "command": "${PLUGIN_ROOT}/hooks/inject-memory.sh"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Write|Edit",
        "hooks": [
          {
            "type": "command",
            "command": "${PLUGIN_ROOT}/hooks/track-changes.sh"
          }
        ]
      }
    ]
  }
}
```

## Plugin Discovery

1. Project-local: `.ion/plugins/`
2. User-global: `~/.config/ion/plugins/`
3. Explicit: `--plugin-dir /path/to/plugin`

## MCP Integration

Plugins can include MCP servers for explicit tool calls:

```json
{
  "mcp": {
    "command": "bun",
    "args": ["run", "${PLUGIN_ROOT}/mcp/server.ts"],
    "env": {
      "OMENDB_LOCAL": "true"
    }
  }
}
```

## Memory Plugin Requirements

For OmenDB memory plugin to work:

| Requirement           | ion Support               |
| --------------------- | ------------------------- |
| UserPromptSubmit hook | ✅ Inject memories        |
| PostToolUse hook      | ✅ Track file changes     |
| SessionStart hook     | ✅ Load workspace         |
| PreCompact hook       | ✅ Save working memory    |
| MCP tools             | ✅ remember, recall, etc. |
| Stdin/stdout JSON     | ✅ Same as Claude Code    |

## Implementation Plan

### Phase 1: Hook System

- [ ] Define HookEvent enum
- [ ] Implement hook runner (subprocess)
- [ ] JSON stdin/stdout protocol
- [ ] Plugin discovery

### Phase 2: Plugin Loading

- [ ] Parse plugin.json
- [ ] Validate hooks configuration
- [ ] Register MCP servers from plugins

### Phase 3: Memory Plugin

- [ ] Port OmenDB memory plugin
- [ ] Or: Use existing TypeScript plugin via Bun

## Compatibility Notes

### Claude Code Plugins

- ✅ `command` hooks (shell scripts)
- ⚠️ `prompt` hooks (need LLM integration)
- ⚠️ `agent` hooks (need subagent support)

### OpenCode Plugins

- ❌ Different format (JS/TS modules)
- Could add adapter layer later

### pi-mono Extensions

- ❌ Different format (TS with AgentTool)
- Could add adapter layer later

## Environment Variables

Hooks receive:

- `PLUGIN_ROOT` - Plugin directory path
- `ION_SESSION_ID` - Current session
- `ION_CWD` - Working directory
- `ION_MODEL` - Current model
- `ION_PROVIDER` - Current provider

## Security

- Hooks run in subprocess (isolated)
- No direct memory access to ion
- Timeout prevents hangs
- Exit 2 can block dangerous operations
