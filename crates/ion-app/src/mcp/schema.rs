//! Output conformance annotates, but never replaces, the server's observation.
use ion_core::{
    ToolResultSchema,
    json_schema::{CompiledSchema, Issue, ValidationFailure},
};
use rmcp::model::CallToolResult;
use serde::Serialize;
use serde_json::Value;

pub(super) struct OutputSchema {
    source: Value,
    validator: Result<CompiledSchema, String>,
}

impl OutputSchema {
    pub(super) fn capture(source: Value) -> Self {
        let validator = CompiledSchema::compile(&source);
        Self { source, validator }
    }

    pub(super) fn unchecked_reason(&self) -> Option<&str> {
        self.validator.as_ref().err().map(String::as_str)
    }

    pub(super) fn hint(&self) -> ToolResultSchema {
        ToolResultSchema {
            value_path: vec!["structured_content".into()],
            schema: self.source.clone(),
        }
    }

    pub(super) fn validate(&self, result: &CallToolResult) -> OutputValidation {
        if result.is_error.unwrap_or(false) {
            return OutputValidation::SkippedRemoteError;
        }
        let validator = match &self.validator {
            Ok(validator) => validator,
            Err(reason) => {
                return OutputValidation::NotChecked {
                    reason: reason.clone(),
                };
            }
        };
        let Some(value) = &result.structured_content else {
            return OutputValidation::Invalid {
                issue: Issue {
                    keyword: "structuredContent".into(),
                    instance_path: Some(String::new()),
                    schema_path: None,
                },
            };
        };
        match validator.validate(value) {
            Ok(()) => OutputValidation::Valid,
            Err(ValidationFailure::Invalid { issue }) => OutputValidation::Invalid { issue },
            Err(ValidationFailure::Indeterminate { issue }) => {
                OutputValidation::Indeterminate { issue }
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum OutputValidation {
    Valid,
    Invalid { issue: Issue },
    Indeterminate { issue: Issue },
    NotChecked { reason: String },
    SkippedRemoteError,
}

impl OutputValidation {
    pub(super) fn needs_raw_result(&self) -> bool {
        matches!(
            self,
            Self::Invalid { .. } | Self::Indeterminate { .. } | Self::NotChecked { .. }
        )
    }
}

#[cfg(test)]
mod tests;
