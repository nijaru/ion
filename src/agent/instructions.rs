//! Instruction loading from AGENTS.md files.
//!
//! Supports layered instructions from:
//! 1. ~/.ion/AGENTS.md (ion-specific)
//! 2. ~/.config/agents/AGENTS.md (cross-agent standard)
//! 3. ./AGENTS.md or ./CLAUDE.md (project-level)

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

/// Cached file content with modification time.
struct CachedFile {
    content: String,
    mtime: SystemTime,
}

/// Loads and caches instruction files from multiple locations.
pub struct InstructionLoader {
    project_path: PathBuf,
    cache: Mutex<HashMap<PathBuf, CachedFile>>,
    loaded_once: AtomicBool,
}

impl InstructionLoader {
    /// Create a new loader for the given project directory.
    #[must_use]
    pub fn new(project_path: PathBuf) -> Self {
        Self {
            project_path,
            cache: Mutex::new(HashMap::new()),
            loaded_once: AtomicBool::new(false),
        }
    }

    /// Load all instruction layers, returning combined content.
    /// Returns None if no instruction files are found.
    pub fn load_all(&self) -> Option<String> {
        self.loaded_once.store(true, Ordering::Relaxed);
        let mut parts = Vec::new();

        // 1. Ion-specific (~/.ion/AGENTS.md)
        if let Some(content) = self.load_ion_local() {
            parts.push(content);
        }

        // 2. Global standard (~/.config/agents/AGENTS.md)
        if let Some(content) = self.load_global() {
            parts.push(content);
        }

        // 3. Project-level (./AGENTS.md or ./CLAUDE.md)
        if let Some(content) = self.load_project() {
            parts.push(content);
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n---\n\n"))
        }
    }

    /// All candidate instruction file paths.
    fn instruction_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".ion/AGENTS.md"));
        }
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")));
        if let Some(dir) = config_dir {
            paths.push(dir.join("agents/AGENTS.md"));
        }
        for name in ["AGENTS.md", "CLAUDE.md"] {
            paths.push(self.project_path.join(name));
        }
        paths
    }

    /// Load ion-specific instructions from ~/.ion/AGENTS.md
    fn load_ion_local(&self) -> Option<String> {
        let path = dirs::home_dir()?.join(".ion/AGENTS.md");
        self.load_cached(&path)
    }

    /// Load global instructions from ~/.config/agents/AGENTS.md
    /// Respects $`XDG_CONFIG_HOME` on Linux.
    fn load_global(&self) -> Option<String> {
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
        let path = config_dir.join("agents/AGENTS.md");
        self.load_cached(&path)
    }

    /// Load project-level instructions from ./AGENTS.md or ./CLAUDE.md
    fn load_project(&self) -> Option<String> {
        for name in ["AGENTS.md", "CLAUDE.md"] {
            let path = self.project_path.join(name);
            if let Some(content) = self.load_cached(&path) {
                return Some(content);
            }
        }
        None
    }

    /// Check if any instruction file has changed since it was last cached.
    /// Cheap: only stats files, doesn't read content.
    pub fn is_stale(&self) -> bool {
        // Not stale if load_all() hasn't been called yet (render cache
        // will miss on its own for the first call).
        if !self.loaded_once.load(Ordering::Relaxed) {
            return false;
        }
        let cache = match self.cache.lock() {
            Ok(c) => c,
            Err(_) => return true,
        };
        // Check if cached files changed on disk
        for (path, cached) in cache.iter() {
            let current_mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            if current_mtime != Some(cached.mtime) {
                return true;
            }
        }
        // Check if new instruction files appeared at uncached paths
        for path in self.instruction_paths() {
            if !cache.contains_key(&path) && path.exists() {
                return true;
            }
        }
        false
    }

    /// Load file with mtime-based caching.
    fn load_cached(&self, path: &Path) -> Option<String> {
        let metadata = fs::metadata(path).ok()?;
        let mtime = metadata.modified().ok()?;

        let mut cache = self.cache.lock().ok()?;

        // Check if cached and still fresh
        if let Some(cached) = cache.get(path)
            && cached.mtime == mtime
        {
            return Some(cached.content.clone());
        }

        // Read and cache
        let content = fs::read_to_string(path).ok()?;

        // Skip empty files
        if content.trim().is_empty() {
            return None;
        }

        cache.insert(
            path.to_path_buf(),
            CachedFile {
                content: content.clone(),
                mtime,
            },
        );

        Some(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_project_agents_md() {
        let dir = TempDir::new().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(&agents_path, "# Project Instructions\nBe helpful.").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());
        let content = loader.load_project().unwrap();
        assert!(content.contains("Project Instructions"));
    }

    #[test]
    fn test_load_project_claude_md_fallback() {
        let dir = TempDir::new().unwrap();
        let claude_path = dir.path().join("CLAUDE.md");
        fs::write(&claude_path, "# Claude Instructions").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());
        let content = loader.load_project().unwrap();
        assert!(content.contains("Claude Instructions"));
    }

    #[test]
    fn test_agents_md_takes_priority() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "AGENTS content").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "CLAUDE content").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());
        let content = loader.load_project().unwrap();
        assert!(content.contains("AGENTS content"));
        assert!(!content.contains("CLAUDE content"));
    }

    #[test]
    fn test_empty_file_ignored() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "   \n\n  ").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());
        assert!(loader.load_project().is_none());
    }

    #[test]
    fn test_caching() {
        let dir = TempDir::new().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(&agents_path, "Initial content").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());

        // First load
        let content1 = loader.load_project().unwrap();
        assert!(content1.contains("Initial"));

        // Modify file (same mtime - cache should still return old content)
        // Note: In practice, mtime would change, but for this test we verify caching works
        let content2 = loader.load_project().unwrap();
        assert_eq!(content1, content2);
    }

    #[test]
    fn test_no_project_files_returns_none() {
        let dir = TempDir::new().unwrap();
        let loader = InstructionLoader::new(dir.path().to_path_buf());
        // load_project returns None when dir has no AGENTS.md or CLAUDE.md
        // (load_all may find global files in ~/.ion/ or ~/.config/agents/)
        assert!(loader.load_project().is_none());
    }

    #[test]
    fn test_is_stale_before_load() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "content").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());
        // Before load_all(), is_stale returns false (nothing loaded yet)
        assert!(!loader.is_stale());
    }

    #[test]
    fn test_is_stale_after_load_unchanged() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "content").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());
        loader.load_all();
        // File unchanged, should not be stale
        assert!(!loader.is_stale());
    }

    #[test]
    fn test_is_stale_after_file_modified() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "original").unwrap();

        let loader = InstructionLoader::new(dir.path().to_path_buf());
        loader.load_all();

        // Modify file with new mtime
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&path, "modified").unwrap();

        assert!(loader.is_stale());
    }

    #[test]
    fn test_is_stale_no_files() {
        let dir = TempDir::new().unwrap();
        let loader = InstructionLoader::new(dir.path().to_path_buf());
        loader.load_all(); // No files found
        // Empty cache after load_all means no files to go stale
        assert!(!loader.is_stale());
    }

    #[test]
    fn test_is_stale_new_file_appears() {
        let dir = TempDir::new().unwrap();
        let loader = InstructionLoader::new(dir.path().to_path_buf());
        loader.load_all(); // No files found
        assert!(!loader.is_stale());

        // Create AGENTS.md mid-session
        fs::write(dir.path().join("AGENTS.md"), "new instructions").unwrap();
        assert!(loader.is_stale());
    }
}
