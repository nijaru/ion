# OmenDB Rust API Research

**Date**: 2026-01-12
**Version**: 0.0.23
**Source**: [docs.rs/omendb](https://docs.rs/omendb/latest/omendb/) | [github.com/omendb/omendb](https://github.com/omendb/omendb)

## Summary

OmenDB is a fast embedded vector database with HNSW + ACORN-1 filtered search. Fully native Rust implementation with no server required. Supports persistence, filtered search, hybrid search (vector + BM25), and quantization (4-8x compression).

**Key traits**: `Send + Sync` on all core types - safe for concurrent access.

---

## Core Types

### VectorStore

Main interface for vector storage and search.

```rust
use omendb::{Vector, VectorStore, VectorStoreOptions, MetadataFilter};
use serde_json::json;

// In-memory store
let mut store = VectorStore::new(768);  // 768 dimensions

// Persistent store
let mut store = VectorStore::open("./vectors.oadb")?;

// Builder pattern with options
let store = VectorStoreOptions::default()
    .dimensions(768)
    .m(32)                    // HNSW neighbors per node (4-64, default 16)
    .ef_construction(200)    // Build quality (default 100)
    .ef_search(100)          // Search quality/speed tradeoff (default 100)
    .metric("cosine")?       // "l2", "cosine", or "dot"
    .quantization_sq8()      // 4x compression, ~99% recall
    .text_search(true)       // Enable hybrid BM25 search
    .open("./vectors")?;
```

**Fields (public)**:

- `vectors: Vec<Vector>` - In-memory vector storage
- `hnsw_index: Option<HNSWIndex>` - HNSW index
- `id_to_index: FxHashMap<String, usize>` - String ID to internal index

### VectorStore Methods

#### Constructors

```rust
fn new(dimensions: usize) -> Self
fn new_with_quantization(dimensions: usize, mode: QuantizationMode) -> Self
fn with_hnsw(dimensions: usize, m: usize, ef_construction: usize, ef_search: usize) -> Result<Self>
fn open(path: impl AsRef<Path>) -> Result<Self>
fn open_with_dims(path: impl AsRef<Path>, dimensions: usize) -> Result<Self>
fn open_with_options(path: impl AsRef<Path>, options: &VectorStoreOptions) -> Result<Self>
fn build_with_options(options: &VectorStoreOptions) -> Result<Self>
```

#### Insert/Update

```rust
// Insert single vector with metadata
fn set(&mut self, id: String, vector: Vector, metadata: JsonValue) -> Result<()>

// Insert with text for hybrid search
fn set_with_text(&mut self, id: String, vector: Vector, metadata: JsonValue, text: &str) -> Result<()>

// Batch insert (more efficient)
fn batch_insert(&mut self, items: Vec<(String, Vector, JsonValue)>) -> Result<Vec<usize>>
```

#### Search

```rust
// Basic k-NN search (returns index, distance pairs)
fn knn_search(&self, query: &Vector, k: usize) -> Result<Vec<(usize, f32)>>

// Search with optional filter
fn search(&mut self, query: &Vector, k: usize, filter: Option<&MetadataFilter>)
    -> Result<Vec<(usize, f32, JsonValue)>>

// Search with ef override and max_distance
fn search_with_options(
    &mut self,
    query: &Vector,
    k: usize,
    filter: Option<&MetadataFilter>,
    ef: Option<usize>,
    max_distance: Option<f32>,
) -> Result<Vec<(usize, f32, JsonValue)>>

// Hybrid vector + text search
fn hybrid_search(
    &mut self,
    query: &Vector,
    text_query: &str,
    k: usize,
    filter: Option<&MetadataFilter>,
) -> Result<Vec<(usize, f32, JsonValue)>>
```

#### CRUD Operations

```rust
fn get(&self, id: &str) -> Option<(&Vector, JsonValue)>
fn delete(&mut self, id: &str) -> Result<()>
fn len(&self) -> usize
fn count(&self) -> usize  // alias for len()
fn is_empty(&self) -> bool
fn ids(&self) -> Vec<String>  // All non-deleted IDs
fn items(&self) -> Vec<(String, Vec<f32>, JsonValue)>  // All items
```

#### Optimization

```rust
fn optimize(&mut self) -> Result<usize>  // BFS reorder for cache efficiency
fn enable_text_search(&mut self) -> Result<()>
fn enable_text_search_with_config(&mut self, config: Option<TextSearchConfig>) -> Result<()>
```

---

### Vector

Simple wrapper around `Vec<f32>`.

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Vector {
    pub data: Vec<f32>,  // Typically 768 or 1536 dimensions
}

impl Vector {
    fn new(data: Vec<f32>) -> Self
    fn dim(&self) -> usize
    fn l2_distance(&self, other: &Vector) -> Result<f32>
    fn dot_product(&self, other: &Vector) -> Result<f32>
    fn cosine_distance(&self, other: &Vector) -> Result<f32>
    fn l2_norm(&self) -> f32
    fn normalize(&self) -> Result<Vector>
}
```

---

### MetadataFilter

MongoDB-style filter operators for filtered search. Uses Roaring bitmaps for O(1) evaluation.

```rust
pub enum MetadataFilter {
    Eq(String, JsonValue),      // field == value
    Ne(String, JsonValue),      // field != value
    Gt(String, f64),            // field > value
    Gte(String, f64),           // field >= value
    Lt(String, f64),            // field < value
    Lte(String, f64),           // field <= value
    In(String, Vec<JsonValue>), // field in [values]
    Contains(String, String),   // field.contains(substring)
    And(Vec<MetadataFilter>),   // All must match
    Or(Vec<MetadataFilter>),    // At least one must match
}

impl MetadataFilter {
    fn from_json(value: &JsonValue) -> Result<Self, String>
    fn matches(&self, metadata: &JsonValue) -> bool
    fn and(self, other: MetadataFilter) -> Self
    fn evaluate_bitmap(&self, index: &MetadataIndex) -> Option<RoaringBitmap>
}
```

**JSON Syntax**:

```rust
// Simple equality
let filter = MetadataFilter::from_json(&json!({"category": "books"}))?;

// Operators
let filter = MetadataFilter::from_json(&json!({
    "price": {"$gte": 10.0, "$lt": 50.0},
    "category": {"$in": ["books", "movies"]}
}))?;

// Logical operators
let filter = MetadataFilter::from_json(&json!({
    "$or": [
        {"type": "article"},
        {"year": {"$gte": 2024}}
    ]
}))?;
```

---

### Metric (Distance Functions)

```rust
#[repr(u8)]
pub enum Metric {
    L2 = 0,      // Euclidean distance (default)
    Cosine = 1,  // 1 - cosine similarity
    Dot = 2,     // Inner product (for MIPS)
}

impl Metric {
    fn parse(s: &str) -> Result<Self, String>
    // Accepts: "l2", "euclidean", "cosine", "dot", "ip"

    fn as_str(&self) -> &'static str
}
```

---

### VectorStoreOptions (Builder)

```rust
VectorStoreOptions::default()
    .dimensions(768)           // Vector dimensionality
    .m(16)                     // HNSW M (4-64)
    .ef_construction(100)      // Build quality
    .ef_search(100)            // Search quality
    .metric("cosine")?         // Distance function
    .quantization_sq8()        // 4x compression
    .quantization_rabitq()     // 8x compression
    .rescore(true)             // Enable rescore with original vectors
    .oversample(3.0)           // Oversample factor for rescore
    .text_search(true)         // Enable hybrid search
    .open("./db")?             // Persistent
    .build()?                  // In-memory
```

---

### QuantizationMode

```rust
pub enum QuantizationMode {
    SQ8,      // 4x compression, ~99% recall (recommended)
    RaBitQ,   // 8x compression, 93-99% recall
    Binary,   // 32x compression, lower recall
}
```

---

### SearchResult (Legacy Type)

```rust
pub struct SearchResult {
    pub id: VectorID,            // u64
    pub distance: f32,
    pub metadata: Option<Vec<u8>>,
}
```

Note: Most search methods return `Vec<(usize, f32, JsonValue)>` tuples directly.

---

## Complete Usage Example

```rust
use omendb::{Vector, VectorStore, VectorStoreOptions, MetadataFilter};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    // Create persistent store with quantization
    let mut store = VectorStoreOptions::default()
        .dimensions(768)
        .m(32)
        .ef_search(100)
        .quantization_sq8()
        .open("./vectors.oadb")?;

    // Insert vectors with metadata
    let embedding = Vector::new(vec![0.1; 768]);
    store.set(
        "doc1".to_string(),
        embedding,
        json!({"type": "article", "year": 2024, "tags": ["rust", "ai"]})
    )?;

    // Batch insert (more efficient)
    let items = vec![
        ("doc2".into(), Vector::new(vec![0.2; 768]), json!({"type": "note", "year": 2023})),
        ("doc3".into(), Vector::new(vec![0.3; 768]), json!({"type": "article", "year": 2024})),
    ];
    store.batch_insert(items)?;

    // Search with filter
    let query = Vector::new(vec![0.15; 768]);
    let filter = MetadataFilter::from_json(&json!({
        "type": "article",
        "year": {"$gte": 2024}
    }))?;

    let results = store.search(&query, 10, Some(&filter))?;
    for (idx, distance, metadata) in results {
        println!("idx={}, distance={:.4}, metadata={}", idx, distance, metadata);
    }

    // Get by ID
    if let Some((vec, metadata)) = store.get("doc1") {
        println!("Found: {} dims, {}", vec.dim(), metadata);
    }

    // Delete
    store.delete("doc3")?;

    Ok(())
}
```

---

## Performance Characteristics

**10K vectors, Apple M3 Max** (m=16, ef=100, k=10):

| Dimension | Single QPS | Batch QPS |
| --------- | ---------- | --------- |
| 128D      | 12,000+    | 87,000+   |
| 768D      | 3,800+     | 20,500+   |
| 1536D     | 1,600+     | 6,200+    |

**SIFT-1M** (1M vectors, 128D):

| Machine      | QPS   | Recall |
| ------------ | ----- | ------ |
| i9-13900KF   | 4,591 | 98.6%  |
| Apple M3 Max | 3,216 | 98.4%  |

---

## Gotchas and Notes

1. **Thread Safety**: All types are `Send + Sync`, but `VectorStore` requires `&mut self` for search (internal caching).

2. **Dimension Validation**: Vector dimensions must match store dimensions. Mismatches return `Result::Err`.

3. **Metadata Serialization**: Metadata is stored as JSON (`serde_json::Value`). Binary metadata uses `Vec<u8>` in `SearchResult`.

4. **Filtered Search Fallback**: If filters cannot use bitmap index (e.g., `Contains`, `Ne`), falls back to JSON-based evaluation.

5. **Persistence**: `.open()` creates/loads from disk. All mutations auto-persist via WAL.

6. **Quantization Training**: Quantization modes train on first batch insert. Insert a representative sample first.

7. **Rescore Overhead**: When quantization enabled, rescore fetches `k * oversample_factor` candidates and reranks.

8. **Text Search**: Requires explicit `enable_text_search()` or `text_search(true)` in options.

---

## Dependencies

Key dependencies from Cargo.toml:

- `anyhow` - Error handling
- `arc-swap` - Concurrent data structures
- `chrono` - Timestamps
- `crc32fast` - Checksums
- `fs2` - File locking
- `lru` - LRU cache
- `memmap2` - Memory-mapped I/O
- `roaring` - Bitmap indexes
- `rustc-hash` (FxHashMap) - Fast hashing
- `serde`, `serde_json` - Serialization
- `tantivy` - Full-text search
- `thiserror` - Error types

---

## For Aircher TUI Agent Integration

**Recommended pattern**:

```rust
use omendb::{VectorStore, VectorStoreOptions, Vector, MetadataFilter};

pub struct ContextualMemory {
    store: VectorStore,
}

impl ContextualMemory {
    pub fn new(path: &Path) -> Result<Self> {
        let store = VectorStoreOptions::default()
            .dimensions(1536)  // OpenAI embeddings
            .m(16)
            .ef_search(100)
            .quantization_sq8()  // 4x compression
            .metric("cosine")?
            .text_search(true)   // Enable hybrid search
            .open(path)?;
        Ok(Self { store })
    }

    pub fn add(&mut self, id: &str, embedding: &[f32], text: &str, metadata: JsonValue) -> Result<()> {
        self.store.set_with_text(
            id.to_string(),
            Vector::new(embedding.to_vec()),
            metadata,
            text,
        )
    }

    pub fn search(&mut self, query_vec: &[f32], query_text: &str, k: usize) -> Result<Vec<SearchHit>> {
        let results = self.store.hybrid_search(
            &Vector::new(query_vec.to_vec()),
            query_text,
            k,
            None,
        )?;
        // Transform results...
    }
}
```

**Key design decisions**:

- Use `quantization_sq8()` for memory efficiency with minimal recall loss
- Use `cosine` metric for normalized embeddings (most embedding models)
- Enable `text_search` for hybrid retrieval
- Store path-based IDs for file context (`src/main.rs:42`)
