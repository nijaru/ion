# Tool Strategy for AI Coding Agent

**Last Updated**: 2025-11-12
**Purpose**: Comprehensive tool selection, bundling, and usage strategy for Aircher

## Executive Summary

**Bundle**: ripgrep + ast-grep (critical for AI performance)
**Assume with Fallback**: fd, jq, sd, git
**Avoid**: bat, delta, nushell (minimal AI benefit)

## Tool Analysis for AI Agents

### High Value Tools (Clear AI Benefits)

#### **ripgrep (rg) vs grep**
- **AI Advantage**: 10-100x faster search = more iterations, better context gathering
- **Use Cases**: Large codebase search, pattern finding, multi-file analysis
- **Decision**: **BUNDLE** - Critical for performance, version consistency
- **Fallback**: grep (when bundled binary unavailable)
- **Performance**: ~10x faster than grep on large codebases

#### **ast-grep vs ripgrep**
- **AI Advantage**: AST-aware = semantic understanding vs text matching
- **Use Cases**: Code refactoring, pattern-based changes, structural queries
- **Decision**: **BUNDLE** - Major advantage for structured operations, not widely installed
- **Fallback**: tree-sitter + ripgrep combination
- **AI Benefit**: Can query "all functions named X" vs regex false positives

#### **tree-sitter (Python bindings)**
- **AI Advantage**: Multi-language syntax parsing, semantic code understanding
- **Use Cases**: Code analysis, syntax highlighting, AST extraction
- **Decision**: **ALWAYS INCLUDE** - Already a Python dependency
- **Integration**: Direct Python API, no external binary
- **Languages**: 19+ supported (Rust, Python, JavaScript, Go, etc.)

#### **fd vs find**
- **AI Advantage**: 5-10x faster file discovery, better gitignore handling
- **Use Cases**: File system navigation, project exploration
- **Decision**: **ASSUME with fallback to find**
- **Rationale**: Performance boost but find works adequately
- **Size**: ~3MB if bundled

#### **jq vs Python json**
- **AI Advantage**: Concise JSON operations, less code to generate
- **Use Cases**: API responses, config files, data extraction
- **Decision**: **ASSUME with fallback to Python**
- **Rationale**: Commonly installed, Python fallback excellent
- **AI Context**: jq syntax is well-known by models

#### **sd vs sed**
- **AI Advantage**: Safer regex (no backslash escaping), modern syntax
- **Use Cases**: Stream editing, file modifications
- **Decision**: **ASSUME with fallback to sed**
- **Rationale**: sed is universal, sd not critical
- **Benefit**: Simpler for AI to generate correct commands

### Low Value Tools (Minimal AI Benefit)

#### **bat vs cat**
- **AI Advantage**: None (syntax highlighting is visual only)
- **Use Case**: File content display
- **Decision**: **DON'T INCLUDE** - AI doesn't benefit from visual formatting
- **Alternative**: Use tree-sitter for syntax analysis, not display

#### **delta vs git diff**
- **AI Advantage**: None (better diff presentation is visual only)
- **Use Case**: Change visualization
- **Decision**: **DON'T INCLUDE** - AI processes structured data, not visual diffs
- **Alternative**: git diff with --unified for structured output

#### **nushell vs bash/Python**
- **AI Consideration**:
  - **Pros**: Structured data pipelines, type safety
  - **Cons**: Smaller AI knowledge base, different syntax, learning curve
  - **Complexity**: Heavy dependency (~50MB), non-standard
- **Decision**: **DON'T INCLUDE** - Python is sufficient for data processing
- **Rationale**: AI models have less nushell training data, Python more reliable

## Bundling Strategy

### Bundle These Tools

```python
BUNDLED_TOOLS = {
    "ripgrep": {
        "reason": "Critical for performance, 10-100x faster search",
        "size": "~5MB",
        "platform": "Single binary, cross-platform",
        "version": "14.1.0",
        "ai_benefit": "More search iterations, better context gathering"
    },
    "ast-grep": {
        "reason": "Semantic code search, major AI advantage",
        "size": "~10MB",
        "platform": "Single binary, cross-platform",
        "version": "0.25.0",
        "ai_benefit": "AST-aware queries, no regex false positives"
    }
}
```

**Total Bundled Size**: ~15MB (acceptable overhead)

### Assume These Tools (with Fallbacks)

```python
ASSUMED_TOOLS = {
    "fd": {
        "preferred": "fd",
        "fallback": "find",
        "reason": "Fast file discovery, but find works",
        "check": "shutil.which('fd')"
    },
    "jq": {
        "preferred": "jq",
        "fallback": "Python json module",
        "reason": "JSON manipulation, Python fallback excellent"
    },
    "sd": {
        "preferred": "sd",
        "fallback": "sed",
        "reason": "Stream editing, sed universal"
    },
    "git": {
        "preferred": "git",
        "fallback": "Error - git required",
        "reason": "System integration, user config important"
    }
}
```

## Implementation

### Tool Management Architecture

```
~/.aircher/
├── tools/
│   ├── bin/              # Bundled binaries
│   │   ├── rg
│   │   └── ast-grep
│   ├── versions.json     # Track bundled versions
│   └── config.yaml       # User overrides
├── sessions/
└── memory/
```

### Installation Logic

```python
class ToolManager:
    """Manage bundled and external tools."""

    def __init__(self):
        self.aircher_dir = Path.home() / ".aircher"
        self.tools_dir = self.aircher_dir / "tools" / "bin"
        self.tools_dir.mkdir(parents=True, exist_ok=True)

        # Add to PATH for this session
        os.environ["PATH"] = f"{self.tools_dir}:{os.environ['PATH']}"

    def ensure_bundled_tools(self):
        """Download bundled tools if missing."""
        for tool_name, config in BUNDLED_TOOLS.items():
            if not self._tool_exists(tool_name):
                self._download_tool(tool_name, config["version"])

    def get_tool(self, purpose: str) -> str:
        """Get best available tool for purpose."""
        preferred, fallback = ASSUMED_TOOLS[purpose]

        if shutil.which(preferred):
            return preferred
        return fallback
```

### Platform Detection

```python
def get_platform() -> str:
    """Get platform identifier for downloads."""
    system = platform.system().lower()
    machine = platform.machine().lower()

    if system == "darwin":
        return "darwin-arm64" if machine == "arm64" else "darwin-x86_64"
    elif system == "linux":
        return "linux-x86_64" if machine in ["x86_64", "amd64"] else "linux-arm64"
    else:
        raise ValueError(f"Unsupported platform: {system}-{machine}")

def get_download_url(tool: str, version: str) -> str:
    """Get platform-specific download URL."""
    platform_id = get_platform()

    URLS = {
        "ripgrep": {
            "darwin-arm64": f"https://github.com/BurntSushi/ripgrep/releases/download/{version}/ripgrep-{version}-aarch64-apple-darwin.tar.gz",
            "darwin-x86_64": f"https://github.com/BurntSushi/ripgrep/releases/download/{version}/ripgrep-{version}-x86_64-apple-darwin.tar.gz",
            "linux-x86_64": f"https://github.com/BurntSushi/ripgrep/releases/download/{version}/ripgrep-{version}-x86_64-unknown-linux-musl.tar.gz"
        },
        "ast-grep": {
            "darwin-arm64": f"https://github.com/ast-grep/ast-grep/releases/download/{version}/ast-grep-aarch64-apple-darwin.tar.gz",
            "darwin-x86_64": f"https://github.com/ast-grep/ast-grep/releases/download/{version}/ast-grep-x86_64-apple-darwin.tar.gz",
            "linux-x86_64": f"https://github.com/ast-grep/ast-grep/releases/download/{version}/ast-grep-x86_64-unknown-linux-musl.tar.gz"
        }
    }

    return URLS[tool][platform_id]
```

## Usage Patterns

### Search Operations

```python
# Code search with ripgrep (bundled)
def search_code(pattern: str, path: str = ".") -> list[str]:
    """Fast code search using bundled ripgrep."""
    result = subprocess.run(
        ["rg", "--json", pattern, path],
        capture_output=True,
        text=True
    )
    return parse_ripgrep_json(result.stdout)

# Semantic search with ast-grep (bundled)
def search_ast(pattern: str, lang: str) -> list[str]:
    """AST-aware code search."""
    result = subprocess.run(
        ["ast-grep", "--pattern", pattern, "--lang", lang, "--json"],
        capture_output=True,
        text=True
    )
    return json.loads(result.stdout)
```

### File Operations

```python
# File discovery with fd or find fallback
def find_files(pattern: str, path: str = ".") -> list[str]:
    """Find files matching pattern."""
    tool = tool_manager.get_tool("find")

    if tool == "fd":
        result = subprocess.run(
            ["fd", "--type", "f", pattern, path],
            capture_output=True,
            text=True
        )
    else:  # find fallback
        result = subprocess.run(
            ["find", path, "-type", "f", "-name", pattern],
            capture_output=True,
            text=True
        )

    return result.stdout.strip().split("\n")
```

### Data Processing

```python
# JSON processing with jq or Python fallback
def process_json(data: str, query: str) -> any:
    """Process JSON with jq or Python."""
    tool = tool_manager.get_tool("json")

    if tool == "jq":
        result = subprocess.run(
            ["jq", query],
            input=data,
            capture_output=True,
            text=True
        )
        return json.loads(result.stdout)
    else:  # Python fallback
        obj = json.loads(data)
        # Implement simple jq-like query parsing
        return apply_json_query(obj, query)
```

## Benefits

### User Experience
- **Zero Setup**: Bundled tools work out of the box
- **Consistent Behavior**: Same tool versions across all installations
- **No System Pollution**: Isolated in ~/.aircher
- **Cross-Platform**: Works on macOS (Intel/ARM), Linux (x86_64/ARM64)

### Performance
- **10-100x Faster Search**: ripgrep vs grep
- **Semantic Queries**: ast-grep eliminates regex false positives
- **More Iterations**: Faster tools = more context gathering
- **Better Results**: AI can explore more code paths

### Maintainability
- **Version Control**: We control bundled tool versions
- **Easy Updates**: Can update tools independently via ~/.aircher/tools/versions.json
- **Testing**: Consistent environment for testing
- **Security**: Download from official GitHub releases with checksums

## Tool Comparison Matrix

| Task | Preferred | Fallback | Bundled | AI Benefit | Performance |
|------|-----------|----------|---------|------------|-------------|
| Text Search | ripgrep | grep | ✅ Yes | High | 10-100x |
| AST Search | ast-grep | tree-sitter | ✅ Yes | High | Semantic |
| File Finding | fd | find | ❌ No | Medium | 5-10x |
| Stream Edit | sd | sed | ❌ No | Low | Safety |
| JSON Process | jq | Python | ❌ No | Medium | Concise |
| Code Parsing | tree-sitter | - | ✅ Yes (dep) | High | Native |

## Implementation Status

### Completed
- ✅ Tool manager class (`src/aircher/tools/manager.py`)
- ✅ Platform detection
- ✅ Download logic
- ✅ PATH management

### In Progress
- 🔄 Version tracking
- 🔄 Checksum verification
- 🔄 Auto-update system

### TODO
- ⏳ User configuration overrides
- ⏳ Tool health checks
- ⏳ Fallback testing

## Recommendations

### For Development
```bash
# Install recommended tools
brew install ripgrep fd sd jq ast-grep

# Or let Aircher bundle them
uv run aircher install-tools  # Downloads to ~/.aircher/tools/bin
```

### For Production
- Bundle ripgrep + ast-grep (15MB total)
- Assume fd, jq, sd with fallbacks
- Require git (coding agent fundamental)
- Skip bat, delta, nushell (no AI benefit)

### For Users
- Zero setup required (bundled tools work)
- Can install system tools for preference
- Config file for custom tool paths
- Graceful fallbacks for missing tools

## References

- **Tool Implementations**: `src/aircher/tools/manager.py`
- **Bash Wrapper**: `src/aircher/tools/bash.py`
- **File Operations**: `src/aircher/tools/file_ops.py`
- **Architecture Decisions**: `ai/DECISIONS.md` (Tool selection rationale)
