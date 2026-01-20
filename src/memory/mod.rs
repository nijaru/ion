use omendb::{Vector, VectorStore};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod embedding;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Vector error: {0}")]
    Vector(String),

    #[error("Query error: {0}")]
    Query(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Semantic,
    Episodic,
    Procedural,
    Working,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub text: String,
    pub r#type: MemoryType,
    pub metadata: serde_json::Value,
    pub timestamp: i64,
    pub helpful_count: u32,
    pub harmful_count: u32,
    pub last_accessed_at: Option<i64>,
}

use crate::memory::embedding::EmbeddingProvider;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

pub struct IndexingRequest {
    pub text: String,
    pub r#type: MemoryType,
    pub metadata: serde_json::Value,
}

use tokio_util::sync::CancellationToken;

/// A handle to the background indexing worker.
pub struct IndexingWorker {
    tx: mpsc::Sender<IndexingRequest>,
    _cancel_token: CancellationToken,
}

impl IndexingWorker {
    pub fn new(memory: Arc<Mutex<MemorySystem>>, embedding: Arc<dyn EmbeddingProvider>) -> Self {
        let (tx, mut rx) = mpsc::channel::<IndexingRequest>(1024);
        let cancel_token = CancellationToken::new();
        let memory_for_flush = memory.clone();
        let flush_cancel = cancel_token.clone();

        // Background flusher
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut ms = memory_for_flush.lock().await;
                        if let Err(e) = ms.flush() {
                            tracing::error!("Background indexing: Flush failed: {}", e);
                        }
                    }
                    _ = flush_cancel.cancelled() => break,
                }
            }
        });

        let worker_cancel = cancel_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    req_opt = rx.recv() => {
                        let Some(req) = req_opt else { break };
                        // Perform embedding
                        let vector = match embedding.embed(&req.text).await {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::error!("Background indexing: Embedding failed: {}", e);
                                continue;
                            }
                        };

                        // Write to memory system
                        let memory = memory.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            let mut ms = memory.blocking_lock();
                            ms.add_entry(&req.text, req.r#type, vector, req.metadata)?;
                            Ok::<(), MemoryError>(())
                        })
                        .await;

                        if let Err(e) = result {
                            tracing::error!("Background indexing: Blocking task panicked: {}", e);
                        } else if let Ok(Err(e)) = result {
                            tracing::error!("Background indexing: Storage failed: {}", e);
                        }
                    }
                    _ = worker_cancel.cancelled() => break,
                }
            }
        });

        Self {
            tx,
            _cancel_token: cancel_token,
        }
    }

    pub async fn index(
        &self,
        text: String,
        r#type: MemoryType,
        metadata: serde_json::Value,
    ) -> Result<(), mpsc::error::SendError<IndexingRequest>> {
        self.tx
            .send(IndexingRequest {
                text,
                r#type,
                metadata,
            })
            .await
    }
}

pub struct MemorySystem {
    store: VectorStore,
    db: rusqlite::Connection,
    #[allow(dead_code)]
    storage_path: PathBuf,
    dimension: usize,
}

impl MemorySystem {
    pub fn new(path: &Path, dimension: usize) -> Result<Self, MemoryError> {
        let storage_path = path.to_path_buf();
        if !storage_path.exists() {
            std::fs::create_dir_all(&storage_path)?;
        }

        let db_path = storage_path.join("memory.db");
        let db = rusqlite::Connection::open(db_path)?;

        // Initialize SQLite schema
        db.execute(
            "CREATE TABLE IF NOT EXISTS entries (
                id TEXT PRIMARY KEY,
                text TEXT NOT NULL,
                type TEXT NOT NULL,
                metadata TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                helpful_count INTEGER DEFAULT 0,
                harmful_count INTEGER DEFAULT 0,
                last_accessed_at INTEGER
            )",
            [],
        )?;

        // Open OmenDB store with persistence
        let vectors_dir = storage_path.join("vectors");
        let vectors_dir_str = vectors_dir.to_str().ok_or_else(|| {
            MemoryError::Storage("Vector storage path contains invalid UTF-8".to_string())
        })?;

        let mut store = VectorStore::open_with_dimensions(vectors_dir_str, dimension)
            .map_err(|e| MemoryError::Vector(e.to_string()))?;

        store
            .enable_text_search()
            .map_err(|e| MemoryError::Vector(e.to_string()))?;

        Ok(Self {
            store,
            db,
            storage_path,
            dimension,
        })
    }

    pub fn add_entry(
        &mut self,
        text: &str,
        r#type: MemoryType,
        vector: Vec<f32>,
        mut metadata: serde_json::Value,
    ) -> Result<String, MemoryError> {
        if vector.len() != self.dimension {
            return Err(MemoryError::Vector(format!(
                "Expected dimension {}, got {}",
                self.dimension,
                vector.len()
            )));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().timestamp();

        // Inject ID into metadata so we can recover it from VectorStore search results if needed
        if let serde_json::Value::Object(ref mut map) = metadata {
            map.insert("_id".to_string(), serde_json::json!(id));
        }

        // Store metadata and text in SQLite
        let meta_str = serde_json::to_string(&metadata)?;
        let type_str = match r#type {
            MemoryType::Semantic => "semantic",
            MemoryType::Episodic => "episodic",
            MemoryType::Procedural => "procedural",
            MemoryType::Working => "working",
        };

        self.db.execute(
            "INSERT INTO entries (id, text, type, metadata, timestamp, helpful_count, harmful_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, text, type_str, meta_str, timestamp, 0, 0],
        )?;

        // Store vector in OmenDB
        let vec = Vector::new(vector);
        self.store
            .set(id.clone(), vec, metadata)
            .map_err(|e| MemoryError::Vector(e.to_string()))?;

        Ok(id)
    }

    pub fn flush(&mut self) -> Result<(), MemoryError> {
        self.store
            .flush()
            .map_err(|e| MemoryError::Vector(e.to_string()))
    }

    pub fn score_entry(&self, entry: &MemoryEntry, relevance_score: f32) -> f32 {
        // relevance_score is already combined (RRF or similarity)
        let mut score = relevance_score;

        // 1. Time decay
        let now = chrono::Utc::now().timestamp();
        let age_minutes = (now - entry.timestamp) / 60;
        let half_life = match entry.r#type {
            MemoryType::Semantic => 10080,   // 7 days
            MemoryType::Episodic => 1440,    // 24 hours
            MemoryType::Working => 60,       // 1 hour
            MemoryType::Procedural => 43200, // 30 days
        };
        let time_decay = (-0.693 * age_minutes as f32 / half_life as f32).exp();
        score *= time_decay;

        // 2. Type weight
        let type_weight = match entry.r#type {
            MemoryType::Working => 2.0,
            MemoryType::Semantic => 1.5,
            MemoryType::Episodic => 1.0,
            MemoryType::Procedural => 1.2,
        };
        score *= type_weight;

        // 3. ACE boost
        let helpful = entry.helpful_count as f32;
        let harmful = entry.harmful_count as f32;
        let ace_boost = if helpful > harmful {
            (1.0 + (helpful - harmful) * 0.1).min(1.5)
        } else if harmful > helpful {
            (1.0 - (harmful - helpful) * 0.2).max(0.3)
        } else {
            1.0
        };
        score *= ace_boost;

        // 4. Recency boost
        if let Some(last_accessed) = entry.last_accessed_at {
            let access_age = (now - last_accessed) / 60;
            if access_age < 60 {
                score *= 1.1;
            }
        }

        score
    }

    /// Prune old memories based on retention policy.
    /// Semantic: 7 days, Episodic: 24 hours, Working: 1 hour.
    pub fn prune(&mut self) -> Result<usize, MemoryError> {
        let now = chrono::Utc::now().timestamp();

        // Retention periods in seconds
        let semantic_retention = 7 * 24 * 60 * 60; // 7 days
        let episodic_retention = 24 * 60 * 60; // 24 hours
        let working_retention = 60 * 60; // 1 hour

        // Find IDs to prune
        let mut stmt = self.db.prepare(
            "SELECT id FROM entries WHERE 
                (type = 'semantic' AND timestamp < ?1) OR
                (type = 'episodic' AND timestamp < ?2) OR
                (type = 'working' AND timestamp < ?3)",
        )?;

        let ids: Vec<String> = stmt
            .query_map(
                rusqlite::params![
                    now - semantic_retention,
                    now - episodic_retention,
                    now - working_retention
                ],
                |row| row.get(0),
            )?
            .filter_map(|r| r.ok())
            .collect();

        let count = ids.len();
        for id in &ids {
            // Delete from SQLite
            self.db.execute("DELETE FROM entries WHERE id = ?1", [id])?;

            // Note: OmenDB 0.0.23 doesn't expose a stable delete yet.
            // Since we join with SQLite during search, pruned entries are effectively removed.
        }

        if count > 0 {
            tracing::info!("MemorySystem: Pruned {} old memories", count);
        }

        Ok(count)
    }

    /// Search for entries using hybrid search (vector + BM25).
    pub fn hybrid_search(
        &mut self,
        query_vector: Vec<f32>,
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, f32)>, MemoryError> {
        let vec = Vector::new(query_vector);

        let results = self
            .store
            .hybrid_search(&vec, query_text, limit * 2, Some(0.5))
            .map_err(|e| MemoryError::Vector(e.to_string()))?;

        let ids_with_dist: Vec<(String, f32)> = results
            .iter()
            .filter_map(|(id, score, _)| Some((id.clone(), *score)))
            .collect();

        if ids_with_dist.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<String> = ids_with_dist.iter().map(|(id, _)| id.clone()).collect();

        // Batch SQLite query
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT id, text, type, metadata, timestamp, helpful_count, harmful_count, last_accessed_at FROM entries WHERE id IN ({})",
            placeholders
        );

        let mut stmt = self.db.prepare(&query)?;
        let entry_iter = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            let id: String = row.get(0)?;
            let text: String = row.get(1)?;
            let type_str: String = row.get(2)?;
            let meta_str: String = row.get(3)?;
            let timestamp: i64 = row.get(4)?;
            let helpful_count: u32 = row.get(5)?;
            let harmful_count: u32 = row.get(6)?;
            let last_accessed_at: Option<i64> = row.get(7)?;

            let r#type: MemoryType = match type_str.as_str() {
                "semantic" => MemoryType::Semantic,
                "episodic" => MemoryType::Episodic,
                "procedural" => MemoryType::Procedural,
                "working" => MemoryType::Working,
                _ => MemoryType::Semantic,
            };

            let metadata: serde_json::Value = serde_json::from_str(&meta_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(MemoryEntry {
                id,
                text,
                r#type,
                metadata,
                timestamp,
                helpful_count,
                harmful_count,
                last_accessed_at,
            })
        })?;

        let mut entries_map: HashMap<String, MemoryEntry> = HashMap::new();
        for entry in entry_iter {
            let entry = entry?;
            entries_map.insert(entry.id.clone(), entry);
        }

        // Score and sort
        let mut scored_entries: Vec<(MemoryEntry, f32)> = ids_with_dist
            .into_iter()
            .filter_map(|(id, relevance_score)| {
                entries_map.get(&id).map(|entry| {
                    let final_score = self.score_entry(entry, relevance_score);
                    (entry.clone(), final_score)
                })
            })
            .collect();

        scored_entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored_entries.truncate(limit);

        Ok(scored_entries)
    }

    /// Search for entries by vector similarity.
    /// Note: This is a blocking operation (rusqlite is synchronous).
    /// Callers in async context should use `spawn_blocking`.
    pub fn search(
        &mut self,
        query_vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, f32)>, MemoryError> {
        let vec = Vector::new(query_vector);

        let results = self
            .store
            .search(&vec, limit * 2, None) // Over-fetch for reranking
            .map_err(|e| MemoryError::Vector(e.to_string()))?;

        let ids_with_dist: Vec<(String, f32)> = results
            .iter()
            .filter_map(|(_, dist, metadata)| {
                metadata
                    .get("_id")
                    .and_then(|v| v.as_str())
                    .map(|s| (s.to_string(), *dist))
            })
            .collect();

        if ids_with_dist.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<String> = ids_with_dist.iter().map(|(id, _)| id.clone()).collect();

        // Batch SQLite query
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT id, text, type, metadata, timestamp, helpful_count, harmful_count, last_accessed_at FROM entries WHERE id IN ({})",
            placeholders
        );

        let mut stmt = self.db.prepare(&query)?;
        let entry_iter = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            let id: String = row.get(0)?;
            let text: String = row.get(1)?;
            let type_str: String = row.get(2)?;
            let meta_str: String = row.get(3)?;
            let timestamp: i64 = row.get(4)?;
            let helpful_count: u32 = row.get(5)?;
            let harmful_count: u32 = row.get(6)?;
            let last_accessed_at: Option<i64> = row.get(7)?;

            let r#type: MemoryType = match type_str.as_str() {
                "semantic" => MemoryType::Semantic,
                "episodic" => MemoryType::Episodic,
                "procedural" => MemoryType::Procedural,
                "working" => MemoryType::Working,
                _ => MemoryType::Semantic,
            };

            let metadata: serde_json::Value = serde_json::from_str(&meta_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(MemoryEntry {
                id,
                text,
                r#type,
                metadata,
                timestamp,
                helpful_count,
                harmful_count,
                last_accessed_at,
            })
        })?;

        let mut entries_map: HashMap<String, MemoryEntry> = HashMap::new();
        for entry in entry_iter {
            let entry = entry?;
            entries_map.insert(entry.id.clone(), entry);
        }

        // Score and sort
        let mut scored_entries: Vec<(MemoryEntry, f32)> = ids_with_dist
            .into_iter()
            .filter_map(|(id, dist)| {
                entries_map.get(&id).map(|entry| {
                    let score = self.score_entry(entry, dist);
                    (entry.clone(), score)
                })
            })
            .collect();

        scored_entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored_entries.truncate(limit);

        Ok(scored_entries)
    }

    /// Add an entry with multiple vectors (ColBERT-style).
    pub fn add_multi_vector_entry(
        &mut self,
        text: &str,
        r#type: MemoryType,
        vectors: Vec<Vec<f32>>,
        mut metadata: serde_json::Value,
    ) -> Result<String, MemoryError> {
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().timestamp();

        if let serde_json::Value::Object(ref mut map) = metadata {
            map.insert("_id".to_string(), serde_json::json!(id));
            map.insert("_doc_id".to_string(), serde_json::json!(id));
        }

        // Store each vector in OmenDB as a separate entry linked by _doc_id
        for (i, vector) in vectors.into_iter().enumerate() {
            if vector.len() != self.dimension {
                return Err(MemoryError::Vector(format!(
                    "Expected dimension {}, got {}",
                    self.dimension,
                    vector.len()
                )));
            }
            let sub_id = format!("{}-{}", id, i);
            let vec = Vector::new(vector);
            self.store
                .set(sub_id, vec, metadata.clone())
                .map_err(|e| MemoryError::Vector(e.to_string()))?;
        }

        // Store metadata and text in SQLite
        let meta_str = serde_json::to_string(&metadata)?;
        let type_str = serde_json::to_string(&r#type)?.replace("\"", "");

        self.db.execute(
            "INSERT INTO entries (id, text, type, metadata, timestamp, helpful_count, harmful_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, text, type_str, meta_str, timestamp, 0, 0],
        )?;

        Ok(id)
    }

    /// Search using multiple query vectors (ColBERT MaxSim approximation).
    pub fn search_multi_vector(
        &mut self,
        query_vectors: Vec<Vec<f32>>,
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, f32)>, MemoryError> {
        let mut doc_scores: HashMap<String, f32> = HashMap::new();

        for q_vec in query_vectors {
            let vec = Vector::new(q_vec);
            // Search for top candidates for this token
            let results = self
                .store
                .search(&vec, limit * 2, None)
                .map_err(|e| MemoryError::Vector(e.to_string()))?;

            // Track max similarity per document for this query token
            let mut current_token_maxsim: HashMap<String, f32> = HashMap::new();
            for (_, dist, metadata) in results {
                if let Some(doc_id) = metadata.get("_doc_id").and_then(|v| v.as_str()) {
                    let similarity = (1.0 - dist).max(0.0);
                    let entry = current_token_maxsim
                        .entry(doc_id.to_string())
                        .or_insert(0.0);
                    if similarity > *entry {
                        *entry = similarity;
                    }
                }
            }

            // Accumulate MaxSim: sum_{q} max_{d} (q . d)
            for (doc_id, max_sim) in current_token_maxsim {
                *doc_scores.entry(doc_id).or_insert(0.0) += max_sim;
            }
        }

        if doc_scores.is_empty() {
            return Ok(Vec::new());
        }

        let mut ids_with_dist: Vec<(String, f32)> = doc_scores.into_iter().collect();
        // Sort by accumulated MaxSim descending
        ids_with_dist.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ids_with_dist.truncate(limit * 2);

        let ids: Vec<String> = ids_with_dist.iter().map(|(id, _)| id.clone()).collect();

        // Batch SQLite query
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT id, text, type, metadata, timestamp, helpful_count, harmful_count, last_accessed_at FROM entries WHERE id IN ({})",
            placeholders
        );

        let mut stmt = self.db.prepare(&query)?;
        let entry_iter = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            let id: String = row.get(0)?;
            let text: String = row.get(1)?;
            let type_str: String = row.get(2)?;
            let meta_str: String = row.get(3)?;
            let timestamp: i64 = row.get(4)?;
            let helpful_count: u32 = row.get(5)?;
            let harmful_count: u32 = row.get(6)?;
            let last_accessed_at: Option<i64> = row.get(7)?;

            let r#type: MemoryType = match type_str.as_str() {
                "semantic" => MemoryType::Semantic,
                "episodic" => MemoryType::Episodic,
                "procedural" => MemoryType::Procedural,
                "working" => MemoryType::Working,
                _ => MemoryType::Semantic,
            };

            let metadata: serde_json::Value = serde_json::from_str(&meta_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(MemoryEntry {
                id,
                text,
                r#type,
                metadata,
                timestamp,
                helpful_count,
                harmful_count,
                last_accessed_at,
            })
        })?;

        let mut entries_map: HashMap<String, MemoryEntry> = HashMap::new();
        for entry in entry_iter {
            let entry = entry?;
            entries_map.insert(entry.id.clone(), entry);
        }

        // Score and sort
        let mut scored_entries: Vec<(MemoryEntry, f32)> = ids_with_dist
            .into_iter()
            .filter_map(|(id, maxsim_score)| {
                entries_map.get(&id).map(|entry| {
                    let final_score = self.score_entry(entry, maxsim_score);
                    (entry.clone(), final_score)
                })
            })
            .collect();

        scored_entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored_entries.truncate(limit);

        Ok(scored_entries)
    }

    pub fn get_entry_by_id(&self, id: &str) -> Result<MemoryEntry, MemoryError> {
        let mut stmt = self.db.prepare("SELECT text, type, metadata, timestamp, helpful_count, harmful_count, last_accessed_at FROM entries WHERE id = ?1")
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        let row = stmt
            .query_row(rusqlite::params![id], |row| {
                let text: String = row.get(0)?;
                let type_str: String = row.get(1)?;
                let meta_str: String = row.get(2)?;
                let timestamp: i64 = row.get(3)?;
                let helpful_count: u32 = row.get(4)?;
                let harmful_count: u32 = row.get(5)?;
                let last_accessed_at: Option<i64> = row.get(6)?;
                Ok((
                    text,
                    type_str,
                    meta_str,
                    timestamp,
                    helpful_count,
                    harmful_count,
                    last_accessed_at,
                ))
            })
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        let (text, type_str, meta_str, timestamp, helpful_count, harmful_count, last_accessed_at) =
            row;
        let metadata: serde_json::Value = serde_json::from_str(&meta_str)?;

        let r#type: MemoryType = match type_str.as_str() {
            "semantic" => MemoryType::Semantic,
            "episodic" => MemoryType::Episodic,
            "procedural" => MemoryType::Procedural,
            "working" => MemoryType::Working,
            _ => MemoryType::Semantic,
        };

        Ok(MemoryEntry {
            id: id.to_string(),
            text,
            r#type,
            metadata,
            timestamp,
            helpful_count,
            harmful_count,
            last_accessed_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_memory_system_basic() {
        let dir = tempdir().unwrap();
        let mut ms = MemorySystem::new(dir.path(), 3).unwrap();

        let id = ms
            .add_entry(
                "hello world",
                MemoryType::Semantic,
                vec![1.0, 0.0, 0.0],
                serde_json::json!({"tag": "test"}),
            )
            .unwrap();

        let results = ms.search(vec![1.0, 0.1, 0.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, id);
        assert_eq!(results[0].0.text, "hello world");
        assert_eq!(results[0].0.metadata["tag"], "test");
    }

    #[tokio::test]
    async fn test_memory_pruning() {
        let dir = tempdir().unwrap();
        let mut ms = MemorySystem::new(dir.path(), 3).unwrap();

        // Add a very old semantic memory (8 days ago)
        let old_semantic_id = uuid::Uuid::new_v4().to_string();
        let old_timestamp = chrono::Utc::now().timestamp() - (8 * 24 * 60 * 60);
        ms.db
            .execute(
                "INSERT INTO entries (id, text, type, metadata, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![old_semantic_id, "old semantic", "semantic", "{}", old_timestamp],
            )
            .unwrap();

        // Add a recent semantic memory
        let recent_id = ms
            .add_entry(
                "recent semantic",
                MemoryType::Semantic,
                vec![0.0, 1.0, 0.0],
                serde_json::json!({}),
            )
            .unwrap();

        // Prune
        let pruned = ms.prune().unwrap();
        assert_eq!(pruned, 1);

        // Verify old is gone, recent remains
        let count: i64 = ms
            .db
            .query_row(
                "SELECT count(*) FROM entries WHERE id = ?1",
                rusqlite::params![old_semantic_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = ms
            .db
            .query_row(
                "SELECT count(*) FROM entries WHERE id = ?1",
                rusqlite::params![recent_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
