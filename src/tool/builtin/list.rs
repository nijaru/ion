use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use ignore::WalkBuilder;
use serde_json::json;
use std::path::Path;

pub struct ListTool;

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &'static str {
        "list"
    }

    fn description(&self) -> &'static str {
        "List contents of a specific directory. Shows files and subdirectories at the given path. For recursive file search by pattern, use glob instead."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list (default: current directory)"
                },
                "depth": {
                    "type": "integer",
                    "description": "Maximum depth to recurse (default: 1 for non-recursive)"
                },
                "type": {
                    "type": "string",
                    "enum": ["file", "dir", "all"],
                    "description": "Filter by type: file, dir, or all (default: all)"
                },
                "hidden": {
                    "type": "boolean",
                    "description": "Include hidden files (default: false)"
                }
            }
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Safe
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        #[allow(clippy::cast_possible_truncation)] // JSON u64 depth value fits in usize
        let depth = args
            .get("depth")
            .and_then(serde_json::Value::as_u64)
            .map_or(1, |d| d as usize);

        let type_filter = args
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("all")
            .to_string();

        let show_hidden = args
            .get("hidden")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        // Resolve path relative to working directory
        let target_path = if Path::new(path).is_absolute() {
            Path::new(path).to_path_buf()
        } else {
            ctx.working_dir.join(path)
        };

        // Check sandbox
        ctx.check_sandbox(&target_path)
            .map_err(ToolError::PermissionDenied)?;

        if !target_path.exists() {
            return Err(ToolError::InvalidArgs(format!(
                "Path does not exist: {path}"
            )));
        }

        if !target_path.is_dir() {
            return Err(ToolError::InvalidArgs(format!(
                "Path is not a directory: {path}"
            )));
        }

        let working_dir = ctx.working_dir.clone();

        let entries = tokio::task::spawn_blocking(move || {
            let walker = WalkBuilder::new(&target_path)
                .hidden(!show_hidden)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .max_depth(Some(depth))
                .build();

            let mut entries = Vec::new();
            for entry in walker.flatten() {
                let entry_path = entry.path();

                // Skip the root directory itself
                if entry_path == target_path {
                    continue;
                }

                // Filter by type
                let is_dir = entry_path.is_dir();
                let include = match type_filter.as_str() {
                    "file" => !is_dir,
                    "dir" => is_dir,
                    _ => true,
                };

                if !include {
                    continue;
                }

                // Format path relative to working directory
                let display_path = entry_path
                    .strip_prefix(&working_dir)
                    .unwrap_or(entry_path)
                    .to_string_lossy()
                    .into_owned();

                // Add trailing slash for directories
                let formatted = if is_dir {
                    format!("{display_path}/")
                } else {
                    display_path
                };

                entries.push(formatted);
            }

            entries.sort();
            entries
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolResult {
            content: if entries.is_empty() {
                "Directory is empty or all contents are ignored.".to_string()
            } else {
                entries.join("\n")
            },
            is_error: false,
            metadata: Some(json!({ "count": entries.len() })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolContext;
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    fn test_context() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            session_id: "test".to_string(),
            abort_signal: CancellationToken::new(),
            no_sandbox: true,
            index_callback: None,
            discovery_callback: None,
        }
    }

    #[tokio::test]
    async fn test_list_src_directory() {
        let tool = ListTool;
        let ctx = test_context();
        let result = tool
            .execute(serde_json::json!({"path": "src"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(
            !result.content.contains("Directory is empty"),
            "src/ should not be empty: {}",
            result.content
        );

        // Should contain known files/dirs
        assert!(
            result.content.contains("main.rs") || result.content.contains("lib.rs"),
            "src/ should contain main.rs or lib.rs: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_list_src_tui_directory() {
        let tool = ListTool;
        let ctx = test_context();
        let result = tool
            .execute(serde_json::json!({"path": "src/tui"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(
            !result.content.contains("Directory is empty"),
            "src/tui/ should not be empty: {}",
            result.content
        );
    }
}
