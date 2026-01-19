use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing exact text. Use for surgical edits instead of rewriting entire files."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to modify"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to replace (must exist in file)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text (must differ from old_string)"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false, requires unique match)"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Restricted
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let file_path_str = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("file_path is required".to_string()))?;

        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("old_string is required".to_string()))?;

        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("new_string is required".to_string()))?;

        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Validation: old_string != new_string
        if old_string == new_string {
            return Err(ToolError::InvalidArgs(
                "old_string and new_string must be different".to_string(),
            ));
        }

        // Validation: old_string not empty (empty old_string = use write tool)
        if old_string.is_empty() {
            return Err(ToolError::InvalidArgs(
                "old_string cannot be empty. Use the write tool to create new files.".to_string(),
            ));
        }

        let file_path = Path::new(file_path_str);
        let validated_path = ctx
            .check_sandbox(file_path)
            .map_err(ToolError::PermissionDenied)?;

        // Validation: file exists
        if !validated_path.exists() {
            return Err(ToolError::InvalidArgs(format!(
                "File not found: {}. Use the write tool to create new files.",
                file_path_str
            )));
        }

        // Read current content
        let content = tokio::fs::read_to_string(&validated_path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {}", e)))?;

        // Count occurrences
        let count = content.matches(old_string).count();

        // Validation: old_string found
        if count == 0 {
            // Show a preview of what we were looking for
            let preview: String = old_string.chars().take(100).collect();
            let suffix = if old_string.len() > 100 { "..." } else { "" };
            return Err(ToolError::InvalidArgs(format!(
                "Text not found in file: \"{}{}\"",
                preview, suffix
            )));
        }

        // Validation: uniqueness (unless replace_all)
        if count > 1 && !replace_all {
            return Err(ToolError::InvalidArgs(format!(
                "Text appears {} times. Use replace_all: true or provide more surrounding context for uniqueness.",
                count
            )));
        }

        // Perform the replacement
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        // Write the file
        tokio::fs::write(&validated_path, &new_content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write file: {}", e)))?;

        // Lazy indexing
        if let Some(callback) = &ctx.index_callback {
            callback(validated_path.clone());
        }

        // Generate diff for output
        let diff = similar::TextDiff::from_lines(&content, &new_content);
        let mut diff_output = String::new();
        for change in diff
            .unified_diff()
            .header(file_path_str, file_path_str)
            .iter_hunks()
        {
            diff_output.push_str(&format!("{}", change));
        }

        let occurrences = if replace_all && count > 1 {
            format!(" ({} occurrences)", count)
        } else {
            String::new()
        };

        let result_msg = format!(
            "Successfully edited {}{}:\n\n```diff\n{}```",
            file_path_str, occurrences, diff_output
        );

        Ok(ToolResult {
            content: result_msg,
            is_error: false,
            metadata: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    fn test_context(dir: &TempDir) -> ToolContext {
        ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: "test".to_string(),
            abort_signal: CancellationToken::new(),
            no_sandbox: false,
            index_callback: None,
            discovery_callback: None,
        }
    }

    #[tokio::test]
    async fn test_edit_simple_replacement() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "Hello world").unwrap();

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "world",
                    "new_string": "Rust"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("Successfully edited"));

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello Rust");
    }

    #[tokio::test]
    async fn test_edit_old_equals_new() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "Hello world").unwrap();

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "world",
                    "new_string": "world"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be different"));
    }

    #[tokio::test]
    async fn test_edit_text_not_found() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "Hello world").unwrap();

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "nonexistent",
                    "new_string": "replacement"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[tokio::test]
    async fn test_edit_multiple_occurrences_fails() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "qux"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("3 times"));
        assert!(err.contains("replace_all"));
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "qux",
                    "replace_all": true
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("3 occurrences"));

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "qux bar qux baz qux");
    }

    #[tokio::test]
    async fn test_edit_file_not_found() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("nonexistent.txt");

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "bar"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("File not found"));
    }

    #[tokio::test]
    async fn test_edit_empty_old_string() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "Hello world").unwrap();

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "",
                    "new_string": "prefix"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_edit_multiline() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        let tool = EditTool;
        let ctx = test_context(&dir);

        let result = tool
            .execute(
                json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "line1\nline2",
                    "new_string": "new1\nnew2"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "new1\nnew2\nline3\n");
    }
}
