//! Ranked discovery shared by model-issued tool_search and Code Mode searchTools.
//! Searches frozen definitions only; ranking never grants execution authority.
use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::ToolDefinition;

const MAX_TEXT_TERMS: usize = 256;
const MAX_SCHEMA_DEPTH: usize = 8;
const K1: f64 = 1.2;
const B: f64 = 0.75;
const NAME_WEIGHT: f64 = 3.0;
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of",
    "on", "or", "that", "the", "this", "to", "with",
];

struct Document<'a> {
    definition: &'a ToolDefinition,
    name_terms: HashSet<String>,
    local_terms: HashSet<String>,
    text_counts: HashMap<String, usize>,
    text_length: usize,
}

impl<'a> Document<'a> {
    fn new(definition: &'a ToolDefinition) -> Self {
        let name = &definition.spec.name;
        let name_terms = tokenize(name).into_iter().collect();
        let local_terms = tokenize(name.rsplit("__").next().unwrap_or(name))
            .into_iter()
            .collect();
        let mut text = Vec::new();
        add_terms(&mut text, &definition.spec.description);
        schema_terms(&definition.spec.input_schema, 0, &mut text);
        let text_length = text.len();
        let mut text_counts = HashMap::new();
        for term in text {
            *text_counts.entry(term).or_insert(0) += 1;
        }
        Self {
            definition,
            name_terms,
            local_terms,
            text_counts,
            text_length,
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.name_terms.contains(term) || self.text_counts.contains_key(term)
    }
}

struct Match<'a> {
    definition: &'a ToolDefinition,
    index: usize,
    exact: bool,
    coverage: usize,
    score: f64,
}

/// BM25 over tool descriptions and argument schemas, with stronger name
/// matches. Covering more query terms takes precedence over raw BM25 score.
/// Ties retain registration order, including tools whose names differ by case.
pub(super) fn rank<'a>(
    query: &str,
    definitions: impl IntoIterator<Item = &'a ToolDefinition>,
    limit: usize,
) -> Vec<&'a ToolDefinition> {
    let mut seen = HashSet::new();
    let terms = tokenize(query)
        .into_iter()
        .filter(|term| seen.insert(term.clone()))
        .collect::<Vec<_>>();
    if terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let documents = definitions
        .into_iter()
        .map(Document::new)
        .collect::<Vec<_>>();
    if documents.is_empty() {
        return Vec::new();
    }
    let count = documents.len() as f64;
    let average_length = documents
        .iter()
        .map(|document| document.text_length)
        .sum::<usize>() as f64
        / count;
    let average_length = average_length.max(1.0);
    let idfs = terms
        .iter()
        .map(|term| {
            let frequency = documents
                .iter()
                .filter(|document| document.contains(term))
                .count() as f64;
            (1.0 + (count - frequency + 0.5) / (frequency + 0.5)).ln()
        })
        .collect::<Vec<_>>();
    let full_name_bonus = idfs.iter().sum::<f64>();
    let exact_name = normalize_name(query);
    let mut matches = Vec::new();

    for (index, document) in documents.iter().enumerate() {
        let norm = K1 * (1.0 - B + B * document.text_length as f64 / average_length);
        let mut coverage = 0;
        let mut score = 0.0;
        for (term, idf) in terms.iter().zip(&idfs) {
            let frequency = document.text_counts.get(term).copied().unwrap_or(0) as f64;
            let text_weight = if frequency > 0.0 {
                frequency * (K1 + 1.0) / (frequency + norm)
            } else {
                0.0
            };
            let name_weight = if document.name_terms.contains(term) {
                NAME_WEIGHT
            } else {
                0.0
            };
            if name_weight > 0.0 || text_weight > 0.0 {
                coverage += 1;
            }
            score += idf * (name_weight + text_weight);
        }
        let name = normalize_name(&document.definition.spec.name);
        let exact = name == exact_name || name.ends_with(&format!("__{exact_name}"));
        if score == 0.0 && !exact {
            continue;
        }
        if !document.local_terms.is_empty()
            && document.local_terms.iter().all(|term| seen.contains(term))
        {
            score += full_name_bonus;
        }
        matches.push(Match {
            definition: document.definition,
            index,
            exact,
            coverage,
            score,
        });
    }
    matches.sort_by(|left, right| {
        right
            .exact
            .cmp(&left.exact)
            .then_with(|| right.coverage.cmp(&left.coverage))
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.index.cmp(&right.index))
    });
    matches
        .into_iter()
        .take(limit)
        .map(|item| item.definition)
        .collect()
}

fn schema_terms(value: &Value, depth: usize, terms: &mut Vec<String>) {
    if depth >= MAX_SCHEMA_DEPTH || terms.len() >= MAX_TEXT_TERMS {
        return;
    }
    if let Some(description) = value.get("description").and_then(Value::as_str) {
        add_terms(terms, description);
    }
    if let Some(properties) = value.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            add_terms(terms, name);
            schema_terms(property, depth + 1, terms);
        }
    }
    if let Some(items) = value.get("items") {
        schema_terms(items, depth + 1, terms);
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(variants) = value.get(key).and_then(Value::as_array) {
            for variant in variants {
                schema_terms(variant, depth + 1, terms);
            }
        }
    }
    if let Some(definitions) = value.get("$defs").and_then(Value::as_object) {
        for (name, definition) in definitions {
            add_terms(terms, name);
            schema_terms(definition, depth + 1, terms);
        }
    }
}

fn add_terms(terms: &mut Vec<String>, text: &str) {
    terms.extend(
        tokenize(text)
            .into_iter()
            .take(MAX_TEXT_TERMS.saturating_sub(terms.len())),
    );
}

fn tokenize(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut previous_lower_or_digit = false;
    for character in text.chars() {
        if !character.is_alphanumeric() {
            push_word(&mut word, &mut result);
            previous_lower_or_digit = false;
            continue;
        }
        if character.is_uppercase() && previous_lower_or_digit {
            push_word(&mut word, &mut result);
        }
        word.extend(character.to_lowercase());
        previous_lower_or_digit = character.is_lowercase() || character.is_numeric();
    }
    push_word(&mut word, &mut result);
    result
}

fn push_word(word: &mut String, result: &mut Vec<String>) {
    if word.is_empty() {
        return;
    }
    let normalized = std::mem::take(word);
    if STOP_WORDS.contains(&normalized.as_str()) {
        return;
    }
    let normalized = if normalized.is_ascii() && normalized.len() > 4 && normalized.ends_with("ies")
    {
        format!("{}y", &normalized[..normalized.len() - 3])
    } else if normalized.is_ascii()
        && normalized.len() > 4
        && ["ches", "shes", "sses", "xes", "zes"]
            .iter()
            .any(|ending| normalized.ends_with(ending))
    {
        normalized[..normalized.len() - 2].to_owned()
    } else if normalized.is_ascii()
        && normalized.len() > 3
        && normalized.ends_with('s')
        && !normalized.ends_with("ss")
    {
        normalized[..normalized.len() - 1].to_owned()
    } else {
        normalized
    };
    result.push(normalized);
}

fn normalize_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '$' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::ToolSpec;
    use serde_json::json;

    fn tool(name: &str, description: &str, schema: Value) -> ToolDefinition {
        ToolDefinition::external(ToolSpec {
            name: name.into(),
            description: description.into(),
            input_schema: schema,
        })
    }

    #[test]
    fn multi_term_coverage_beats_a_single_name_match() {
        let tools = [
            tool("window", "Focus a window", json!({})),
            tool(
                "mcp__desktop__capture",
                "Take a screenshot of a window",
                json!({}),
            ),
        ];
        assert_eq!(
            rank("screenshot window", tools.iter(), 2)
                .iter()
                .map(|item| item.spec.name.as_str())
                .collect::<Vec<_>>(),
            ["mcp__desktop__capture", "window"]
        );
    }

    #[test]
    fn names_schema_and_word_forms_are_searchable_without_stop_word_noise() {
        let tools = [
            tool(
                "mcp__tracker__fetch",
                "Retrieve an item",
                json!({
                    "properties": {"issue_number": {"description": "Repository issue identifier"}}
                }),
            ),
            tool("mcp__tracker__list_users", "List users", json!({})),
        ];
        assert_eq!(rank("a the to", tools.iter(), 5).len(), 0);
        assert_eq!(
            rank("issues", tools.iter(), 5)[0].spec.name,
            "mcp__tracker__fetch"
        );
        assert_eq!(
            rank("issue number", tools.iter(), 5)[0].spec.name,
            "mcp__tracker__fetch"
        );
    }

    #[test]
    fn exact_local_names_and_registration_order_survive_ties() {
        let tools = [
            tool("candidate_00", "Similar discovery candidate_11", json!({})),
            tool("candidate_11", "Similar discovery candidate_11", json!({})),
            tool("CANDIDATE_11", "Similar discovery candidate_11", json!({})),
        ];
        assert_eq!(
            rank("CANDIDATE_11", tools.iter(), 3)
                .iter()
                .map(|item| item.spec.name.as_str())
                .collect::<Vec<_>>(),
            ["candidate_11", "CANDIDATE_11", "candidate_00"]
        );
        assert_eq!(
            rank("similar", tools.iter(), 3)
                .iter()
                .map(|item| item.spec.name.as_str())
                .collect::<Vec<_>>(),
            ["candidate_00", "candidate_11", "CANDIDATE_11"]
        );
        let odd_case = tool("Read", "Unrelated metadata", json!({}));
        assert_eq!(
            rank("rEaD", std::iter::once(&odd_case), 1)[0].spec.name,
            "Read"
        );
    }
}
