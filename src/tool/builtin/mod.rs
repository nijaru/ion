mod bash;
mod discover;
mod glob;
mod grep;
mod read;
mod write;

pub use bash::BashTool;
pub use discover::DiscoverTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

use crate::tool::ToolError;
use std::path::Path;

/// Validates that a path is within the allowed working directory.
/// Returns the canonicalized path if valid.
pub fn validate_path_within_working_dir(
    path: &Path,
    working_dir: &Path,
) -> Result<std::path::PathBuf, ToolError> {
    // Canonicalize working_dir (must exist)
    let canonical_working_dir = working_dir.canonicalize().map_err(|e| {
        ToolError::ExecutionFailed(format!("Failed to resolve working directory: {}", e))
    })?;

    // For the target path, we need to handle non-existent files
    // Canonicalize what exists, then append the rest
    let canonical_path = if path.exists() {
        path.canonicalize()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to resolve path: {}", e)))?
    } else {
        // For non-existent paths, canonicalize the parent and append the filename
        let parent = path.parent().unwrap_or(Path::new("."));
        let filename = path
            .file_name()
            .ok_or_else(|| ToolError::InvalidArgs("Invalid path: no filename".to_string()))?;

        let canonical_parent = if parent.as_os_str().is_empty() || parent == Path::new(".") {
            canonical_working_dir.clone()
        } else {
            parent.canonicalize().map_err(|e| {
                ToolError::ExecutionFailed(format!("Parent directory does not exist: {}", e))
            })?
        };

        canonical_parent.join(filename)
    };

    // Check that the canonical path starts with the working directory
    if !canonical_path.starts_with(&canonical_working_dir) {
        return Err(ToolError::PermissionDenied(format!(
            "Path '{}' is outside the working directory",
            path.display()
        )));
    }

    Ok(canonical_path)
}
