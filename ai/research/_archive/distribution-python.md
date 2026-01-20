# Distribution Strategy Analysis

## Python Distribution Options

### **Option 1: PyPI (Current)**
```bash
pip install aircher
uv add aircher
```
**Pros**: Standard, works everywhere, familiar
**Cons**: Python-only, no Mojo support

### **Option 2: PyPI + Separate Mojo**
```bash
pip install aircher          # Core Python package
aircher install-mojo         # Downloads Mojo components
```
**Pros**: Clean separation, optional Mojo
**Cons**: Two-step installation

### **Option 3: Pixi-First**
```bash
pixi install aircher         # Handles Python + Mojo
```
**Pros**: Native multi-language support
**Cons**: Newer, less adoption

### **Option 4: Container**
```bash
docker run aircher           # All tools included
```
**Pros**: Complete environment
**Cons**: Heavy, isolation issues

## Recommendation: Phased Approach

### **Phase 1**: PyPI + On-demand Downloads
```python
# src/aircher/tools/manager.py
async def ensure_tools():
    """Download tools if missing."""
    for tool in CRITICAL_TOOLS:
        if not shutil.which(tool):
            await download_tool(tool)
```

### **Phase 2**: Add Mojo Support
```bash
# When Mojo 1.0 released
pip install aircher[mojo]    # Extra with Mojo components
```

### **Phase 3**: Pixi Migration (if needed)
```toml
# pixi.toml
[dependencies]
python = ">=3.13"
mojo = ">=1.0"

[project]
dependencies = ["aircher"]
```

## Tool Download Strategy

### **On-Demand Downloads**
```python
class ToolManager:
    async def get_tool(self, name: str) -> str:
        # Check system PATH first
        if shutil.which(name):
            return name

        # Check ~/.aircher/tools/
        local_tool = self.tools_dir / name
        if local_tool.exists():
            return str(local_tool)

        # Download if missing
        await self.download_tool(name)
        return str(local_tool)
```

### **User Experience**
```bash
# First time user
uv run aircher run "help with this code"
# → "Downloading ripgrep for faster search..."
# → "Downloading ast-grep for code analysis..."
# → Agent starts working

# Subsequent uses
uv run aircher run "help with this code"
# → Tools already available, starts immediately
```

## Implementation Details

### **Tool Storage**
```
~/.aircher/
├── tools/
│   ├── bin/
│   │   ├── rg
│   │   ├── ast-grep
│   │   └── .versions.json
│   └── downloads/
├── sessions/
└── memory/
```

### **Download Logic**
```python
TOOL_CONFIGS = {
    "rg": {
        "version": "14.1.0",
        "urls": {
            "darwin-arm64": "https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-aarch64-apple-darwin.tar.gz",
            "darwin-x86_64": "https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-x86_64-apple-darwin.tar.gz",
            "linux-x86_64": "https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz"
        }
    }
}
```

## Migration Path to Mojo

### **When Mojo 1.0 Released**
```python
# src/aircher/performance/__init__.py
try:
    import mojo_perf  # Mojo compiled module
    HAS_MOJO = True
except ImportError:
    HAS_MOJO = False
    import fallback_perf as mojo_perf

def fast_operation(data):
    if HAS_MOJO:
        return mojo_perf.fast_operation(data)
    else:
        return fallback_perf.fast_operation(data)
```

### **Package Structure with Mojo**
```
aircher/
├── src/aircher/           # Python code
├── mojo/                   # Mojo source
├── mojo-built/             # Compiled Mojo extensions
└── pyproject.toml
```

## Final Recommendation

**Current**: PyPI distribution + on-demand tool downloads
**Future**: Add Mojo as optional extra when 1.0 released
**Long-term**: Consider pixi if multi-language needs grow

This gives us:
- ✅ Easy installation now
- ✅ Performance tools always available
- ✅ Clear migration path for Mojo
- ✅ User-friendly zero-setup experience
