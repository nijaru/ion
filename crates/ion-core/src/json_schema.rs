//! Shared, effect-free JSON Schema compilation and strict validation.
//!
//! Compilation is offline (including `$schema`), bounded by document size and
//! reference-expanded work, and accepts only document-local JSON-pointer refs.
//! Recursive/resource/anchor scopes and unevaluated annotation checks are not
//! supported. Patterns use the checked ASCII subset below with a linear engine;
//! accepting a Rust regex alone would not establish ECMAScript semantics.
//! Compilation failure is explicit, never an accept-all validator. Validation
//! neither coerces values nor inserts defaults, removes fields or rewrites JSON.
//! Callers decide whether unavailable/indeterminate contracts refuse dispatch
//! (inputs) or annotate an already observed result (MCP outputs).
use std::collections::{HashMap, HashSet};

use jsonschema::{ValidationError, error::ValidationErrorKind};
use serde::Serialize;
use serde_json::Value;

const MAX_SCHEMA_NODES: usize = 4096;
const MAX_SCHEMA_DEPTH: usize = 64;
const MAX_SCHEMA_TEXT_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 256;

/// One immutable compiled contract, reusable for strict checks of borrowed JSON.
pub struct CompiledSchema(jsonschema::Validator);

impl CompiledSchema {
    /// Errors contain bounded local coordinates, not schema values or URIs.
    pub fn compile(source: &Value) -> Result<Self, String> {
        check_budget(source)?;
        Expansion::default().cost(source, source)?;
        jsonschema::options()
            .offline()
            // Backtracking limits alone are insufficient: exploring all possible
            // unfinished matches can itself be exponential. Unsupported ECMA
            // patterns produce explicit compilation failures, not unsafe checks.
            .with_pattern_options(jsonschema::PatternOptions::regex())
            .build(source)
            .map(Self)
            .map_err(|error| {
                // Library messages can quote schema values or external URIs.
                // Report only the keyword and a bounded local coordinate.
                let location = bounded_path(error.schema_path().as_str());
                format!(
                    "invalid/unsupported schema or unresolved reference (keyword {}, schema path {})",
                    error.kind().keyword(),
                    serde_json::to_string(&location).expect("a path encodes")
                )
            })
    }

    pub fn validate(&self, value: &Value) -> Result<(), ValidationFailure> {
        self.0.validate(value).map_err(|error| {
            let issue = Issue::from_error(&error);
            match error.kind() {
                ValidationErrorKind::BacktrackLimitExceeded { .. }
                | ValidationErrorKind::RegexEngineFailure { .. } => {
                    ValidationFailure::Indeterminate { issue }
                }
                _ => ValidationFailure::Invalid { issue },
            }
        })
    }
}

/// A failed check is either a proven mismatch or an inconclusive engine check.
#[derive(Debug)]
pub enum ValidationFailure {
    Invalid { issue: Issue },
    Indeterminate { issue: Issue },
}

impl std::fmt::Display for ValidationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (status, issue) = match self {
            Self::Invalid { issue } => ("invalid", issue),
            Self::Indeterminate { issue } => ("indeterminate", issue),
        };
        write!(
            f,
            "{status}: {}",
            serde_json::to_string(issue).expect("an issue encodes")
        )
    }
}

impl std::error::Error for ValidationFailure {}

#[derive(Debug, Serialize)]
pub struct Issue {
    pub keyword: String,
    /// Null means unavailable/over bound; the root is the empty string.
    pub instance_path: Option<String>,
    pub schema_path: Option<String>,
}

impl Issue {
    fn from_error(error: &ValidationError<'_>) -> Self {
        Self {
            keyword: error.kind().keyword().to_owned(),
            instance_path: bounded_path(error.instance_path().as_str()),
            schema_path: bounded_path(error.schema_path().as_str()),
        }
    }
}

fn bounded_path(path: &str) -> Option<String> {
    (path.len() <= MAX_PATH_BYTES).then(|| path.to_owned())
}

/// Admit only a shared ASCII regex language: literals, positive word/range
/// classes, anchors and greedy quantifiers. Rust's Unicode boundaries and dot
/// differ from ECMAScript; successful engine compilation is not a semantics proof.
fn supported_pattern(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    let mut repeatable = false;
    let mut quantified = false;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'[' => {
                let start = index + 1;
                index = start;
                while index < bytes.len() && bytes[index] != b']' {
                    if !(bytes[index].is_ascii_alphanumeric()
                        || matches!(bytes[index], b'_' | b'-'))
                    {
                        return false;
                    }
                    index += 1;
                }
                if index == bytes.len() || index == start || pattern[start..index].contains("--") {
                    return false;
                }
                repeatable = true;
                quantified = false;
            }
            b'^' if index == 0 => repeatable = false,
            b'$' if index + 1 == bytes.len() => repeatable = false,
            b'*' | b'+' | b'?' if repeatable && !quantified => quantified = true,
            b'{' if repeatable && !quantified => {
                index += 1;
                let start = index;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
                if index == start {
                    return false;
                }
                if bytes.get(index) == Some(&b',') {
                    index += 1;
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        index += 1;
                    }
                }
                if bytes.get(index) != Some(&b'}') {
                    return false;
                }
                quantified = true;
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') => {
                repeatable = true;
                quantified = false;
            }
            _ => return false,
        }
        index += 1;
    }
    true
}

fn check_budget(schema: &Value) -> Result<(), String> {
    let mut pending = vec![(schema, 0)];
    let mut nodes = 0usize;
    let mut text_bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > MAX_SCHEMA_NODES || depth > MAX_SCHEMA_DEPTH {
            return Err(format!(
                "schema exceeds {MAX_SCHEMA_NODES} JSON nodes or depth {MAX_SCHEMA_DEPTH}"
            ));
        }
        let children = match value {
            Value::Object(values) => values.len(),
            Value::Array(values) => values.len(),
            _ => 0,
        };
        if children > MAX_SCHEMA_NODES - nodes - pending.len() {
            return Err(format!("schema exceeds {MAX_SCHEMA_NODES} JSON nodes"));
        }
        match value {
            Value::Object(values) => {
                // These validators revisit nested branches while computing
                // annotations, even without references. Document/graph size
                // cannot bound that implicit revalidation tree.
                if values.contains_key("unevaluatedProperties")
                    || values.contains_key("unevaluatedItems")
                {
                    return Err(
                        "unevaluated annotation checks are outside bounded validation".into(),
                    );
                }
                let pattern = values.get("pattern").and_then(Value::as_str);
                let patterns = values.get("patternProperties").and_then(Value::as_object);
                if pattern.is_some_and(|pattern| !supported_pattern(pattern))
                    || patterns.is_some_and(|patterns| {
                        patterns.keys().any(|pattern| !supported_pattern(pattern))
                    })
                    || values.get("format").and_then(Value::as_str) == Some("regex")
                {
                    return Err(
                        "pattern semantics are outside the checked ASCII regex subset".into(),
                    );
                }
                // Resolving resource/anchor scopes requires a different graph
                // proof. Do not approximate them or silently ignore a $ref.
                if ["$id", "id", "$anchor", "$dynamicAnchor", "$recursiveAnchor"]
                    .iter()
                    .any(|key| values.get(*key).is_some_and(Value::is_string))
                    || values.contains_key("$dynamicRef")
                    || values.contains_key("$recursiveRef")
                {
                    return Err(
                        "schema resource/anchor scopes are outside bounded validation".into(),
                    );
                }
                for (key, value) in values {
                    text_bytes = text_bytes.saturating_add(key.len());
                    pending.push((value, depth + 1));
                }
            }
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            Value::String(value) => text_bytes = text_bytes.saturating_add(value.len()),
            _ => {}
        }
        if text_bytes > MAX_SCHEMA_TEXT_BYTES {
            return Err(format!("schema exceeds {MAX_SCHEMA_TEXT_BYTES} text bytes"));
        }
    }
    Ok(())
}

/// Bound reference-expanded work, not just the compact document. Counting every
/// JSON child is conservative (including annotations and unused definitions),
/// but cannot mistake a small DAG for a cheap schema. Cycles are unsupported even
/// when instance descent would make some of them terminate: repeated branches
/// can still be exponential in the instance depth.
#[derive(Default)]
struct Expansion {
    visiting: HashSet<*const Value>,
    costs: HashMap<*const Value, (usize, usize)>,
}

impl Expansion {
    fn cost(&mut self, root: &Value, node: &Value) -> Result<(usize, usize), String> {
        let identity = std::ptr::from_ref(node);
        if let Some(cost) = self.costs.get(&identity) {
            return Ok(*cost);
        }
        if self.visiting.len() >= MAX_SCHEMA_DEPTH || !self.visiting.insert(identity) {
            return Err(
                "recursive or deeply expanded references are outside bounded validation".into(),
            );
        }
        let mut children: Vec<_> = match node {
            Value::Object(values) => values.values().collect(),
            Value::Array(values) => values.iter().collect(),
            _ => Vec::new(),
        };
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            let pointer = reference
                .strip_prefix('#')
                .filter(|pointer| {
                    (pointer.is_empty() || pointer.starts_with('/')) && !pointer.contains('%')
                })
                .ok_or_else(|| {
                    "only document-local JSON-pointer references are checked".to_owned()
                })?;
            children.push(
                root.pointer(pointer)
                    .ok_or_else(|| "unresolved document-local reference".to_owned())?,
            );
        }
        let mut nodes = 1usize;
        let mut depth = 1usize;
        for child in children {
            let (child_nodes, child_depth) = self.cost(root, child)?;
            if child_nodes > MAX_SCHEMA_NODES - nodes {
                return Err(format!(
                    "reference expansion exceeds {MAX_SCHEMA_NODES} JSON nodes"
                ));
            }
            nodes += child_nodes;
            depth = depth.max(child_depth + 1);
            if depth > MAX_SCHEMA_DEPTH {
                return Err(format!(
                    "reference expansion exceeds depth {MAX_SCHEMA_DEPTH}"
                ));
            }
        }
        self.visiting.remove(&identity);
        self.costs.insert(identity, (nodes, depth));
        Ok((nodes, depth))
    }
}

#[cfg(test)]
#[path = "json_schema_tests.rs"]
mod tests;
