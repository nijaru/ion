//! ast-grep structural code search tool.
//!
//! Wraps the `sg` binary for AST pattern matching. Requires `sg` to be on PATH.

use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use tokio::process::Command;

/// Maximum output size to return.
const MAX_OUTPUT: usize = 40_000;

pub struct AstGrepTool;

#[async_trait]
impl Tool for AstGrepTool {
    fn name(&self) -> &'static str {
        "ast_grep"
    }

    fn description(&self) -> &'static str {
        "Structural code search using AST patterns (requires `sg` on PATH). Use $VAR for metavariables and $$$ARGS for multiple nodes. More precise than regex for code: finds function definitions, call sites, and structural patterns across a language's syntax tree."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "AST pattern to match. Use $VAR for single-node captures, $$$ARGS for multi-node captures. Examples: 'fn $NAME($$$) -> Result<$$$>', 'console.log($$$)', 'if ($COND) { $$$ }'"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search (default: working directory)"
                },
                "language": {
                    "type": "string",
                    "description": "Source language (rust, python, javascript, typescript, go, java, c, cpp, etc). Auto-detected from file extension if omitted."
                }
            },
            "required": ["pattern"]
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
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("pattern is required".to_string()))?;

        let search_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| {
                let p = Path::new(p);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    ctx.working_dir.join(p)
                }
            })
            .unwrap_or_else(|| ctx.working_dir.clone());

        let validated_path = ctx
            .check_sandbox(&search_path)
            .map_err(ToolError::PermissionDenied)?;

        let language = args.get("language").and_then(|v| v.as_str());

        let mut cmd = Command::new("sg");
        cmd.arg("run")
            .arg("--pattern")
            .arg(pattern)
            .arg("--heading")
            .arg("never");

        if let Some(lang) = language {
            cmd.arg("--lang").arg(lang);
        }

        cmd.arg(&validated_path)
            .current_dir(&ctx.working_dir)
            // Avoid inheriting a colorized terminal
            .env("NO_COLOR", "1");

        let output = cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::ExecutionFailed(
                    "ast-grep (sg) not found on PATH. Install with: cargo install ast-grep"
                        .to_string(),
                )
            } else {
                ToolError::ExecutionFailed(format!("Failed to run sg: {e}"))
            }
        })?;

        // sg exits with code 1 when no matches found (like grep), not an error
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Non-zero exit with stderr and no stdout = real error
        if !output.status.success() && stdout.is_empty() && !stderr.is_empty() {
            return Ok(ToolResult {
                content: format!("ast-grep error: {}", stderr.trim()),
                is_error: true,
                metadata: None,
            });
        }

        let content = if stdout.is_empty() {
            "No matches found.".to_string()
        } else if stdout.len() > MAX_OUTPUT {
            format!(
                "{}\n\n[Output truncated at {} bytes]",
                &stdout[..MAX_OUTPUT],
                MAX_OUTPUT,
            )
        } else {
            stdout.into_owned()
        };

        Ok(ToolResult {
            content,
            is_error: false,
            metadata: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolContext;
    use std::path::PathBuf;

    fn make_ctx() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            session_id: "test".into(),
            abort_signal: tokio_util::sync::CancellationToken::new(),
            no_sandbox: true,
            index_callback: None,
        }
    }

    #[tokio::test]
    async fn test_ast_grep_no_pattern() {
        let tool = AstGrepTool;
        let ctx = make_ctx();
        let result = tool.execute(json!({}), &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ast_grep_basic_search() {
        // Skip if sg not installed
        if std::process::Command::new("sg")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let tool = AstGrepTool;
        let ctx = make_ctx();
        // Search for fn declarations in this file itself
        let result = tool
            .execute(
                json!({
                    "pattern": "fn $NAME",
                    "path": "src/tool/builtin/ast_grep.rs",
                    "language": "rust"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        // Should find at least our test function
        assert!(result.content.contains("ast_grep.rs"));
    }
}
