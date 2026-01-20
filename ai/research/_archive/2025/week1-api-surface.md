# Week 1 API Surface (Implementation Checklist)

**Source**: ai/research/memory-system-architecture.md
**Goal**: Python classes to implement (no verbose code)

---

## DuckDB Episodic Memory

**File**: `src/aircher/memory/duckdb_memory.py`

**Class**: `DuckDBMemory`
```python
class DuckDBMemory:
    __init__(db_path: Path | None)
    record_tool_execution(session_id, task_id, tool_name, parameters, result, success, ...)
    record_file_interaction(session_id, task_id, file_path, operation, line_range, ...)
    get_file_history(file_path, limit=5) -> List[Dict]
    find_co_edit_patterns(min_count=3) -> List[Dict]
    get_tool_statistics(days=7) -> List[Dict]
```

**Schema**: Copy lines 85-166 from memory-system-architecture.md

---

## ChromaDB Vector Search

**File**: `src/aircher/memory/vector_search.py`

**Class**: `VectorSearch`
```python
class VectorSearch:
    __init__(persist_directory: Path | None)
    index_code_snippet(file_path, content, start_line, end_line, language, metadata)
    search(query, n_results=10, filter_language=None) -> List[Dict]
    async index_codebase(root_path, languages)  # Background task
```

**Model**: `sentence-transformers/all-MiniLM-L6-v2` (384 dims, fast)

---

## Knowledge Graph

**File**: `src/aircher/memory/knowledge_graph.py`

**Class**: `KnowledgeGraph`
```python
class KnowledgeGraph:
    __init__()  # Uses NetworkX DiGraph
    add_file(path, language) -> node_id
    add_function(name, signature, line, file_path, file_node_id) -> node_id
    add_call_edge(caller_id, callee_id)
    get_file_contents(file_path) -> Dict  # {functions, classes, imports}
    get_callers(function_name) -> List[str]
    save(path: Path)
    load(path: Path)
```

**Data**: NetworkX graph, serialize with pickle

---

## Tree-sitter Extraction

**File**: `src/aircher/memory/tree_sitter_extractor.py`

**Class**: `TreeSitterExtractor`
```python
class TreeSitterExtractor:
    __init__()  # Load tree_sitter_python, _rust, _javascript
    extract_functions(file_path, language) -> List[Dict]  # {name, signature, line}
    extract_classes(file_path, language) -> List[Dict]
    extract_imports(file_path, language) -> List[Dict]
```

---

## Integration with Agent

**File**: `src/aircher/memory/integration.py`

**Class**: `MemoryIntegration`
```python
class MemoryIntegration:
    __init__(episodic_memory, vector_search, knowledge_graph)
    track_tool_execution(tool_func) -> Callable  # Decorator
    set_context(session_id, task_id)
```

**Usage**:
```python
@memory.track_tool_execution
def read_file(file_path: str) -> str:
    ...
```

---

## Week 1 Deliverables

**Day 1-2**: DuckDB
- [ ] Create schema (copy SQL from research)
- [ ] Implement record methods
- [ ] Test with sample data

**Day 3-4**: ChromaDB
- [ ] Initialize with sentence-transformers
- [ ] Index sample code snippet
- [ ] Test semantic search

**Day 5**: Knowledge Graph
- [ ] NetworkX graph setup
- [ ] Tree-sitter extraction for Python
- [ ] Query file contents

**Day 6-7**: Integration & Tests
- [ ] Decorator for auto-tracking
- [ ] Unit tests (>80% coverage)
- [ ] Validate: Query "Have I seen file X?" works

---

**That's it. No verbose code. Implementation details in research file.**
