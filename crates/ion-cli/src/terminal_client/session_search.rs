//! Search captured Session fields, never their painted labels or live files.
use ion_ai::{Content, Message};
use ion_app::SessionSummary;

pub(super) struct SessionSearch {
    name: String,
    cwd: String,
    id: String,
    preview: String,
}

impl SessionSearch {
    pub fn new(session: &SessionSummary) -> Self {
        Self {
            name: session.name.as_deref().unwrap_or_default().to_lowercase(),
            cwd: session
                .cwd
                .as_ref()
                .map(|path| path.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
            id: session.id.to_lowercase(),
            preview: session
                .preview
                .as_deref()
                .unwrap_or_default()
                .to_lowercase(),
        }
    }

    pub fn named(&self) -> bool {
        !self.name.trim().is_empty()
    }
}

/// A picker-local projection of complete readable message fields. Preparing it
/// once avoids repeatedly normalizing long prompts during filtering and paint.
/// Only plain text, tool names and public kind markers are indexed, not
/// reasoning, image bytes, tool payloads or provider replay.
pub(super) struct MessageSearch(Vec<String>);

impl MessageSearch {
    pub fn new(message: &Message, metadata: &str) -> Self {
        let mut fields = vec![normalize_whitespace(&metadata.to_lowercase())];
        fields.extend(message.content.iter().filter_map(|part| {
            let text = match part {
                Content::Text(text) => text.as_str(),
                Content::ToolCall(call) => call.name.as_str(),
                Content::ToolResult(result) => result.name.as_str(),
                Content::Image(_) => "[image]",
                Content::Thinking(_) => return None,
            };
            (!text.trim().is_empty()).then(|| normalize_whitespace(&text.to_lowercase()))
        }));
        if fields.len() == 1 {
            fields.push("[no text]".into());
        }
        Self(fields)
    }

    pub fn matches(&self, query: &SessionQuery) -> bool {
        query.0.iter().all(|token| {
            let (Token::Word(text) | Token::Phrase(text)) = token;
            self.0.iter().any(|field| field.contains(text))
        })
    }
}

enum Token {
    Word(String),
    Phrase(String),
}

pub(super) struct SessionQuery(Vec<Token>);

impl SessionQuery {
    pub fn new(query: &str) -> Self {
        // Like Pi, an unfinished quote is literal whitespace-delimited input,
        // not an error that closes the browser or changes the editor source.
        if query.chars().filter(|ch| *ch == '"').count() % 2 != 0 {
            return Self(
                query
                    .split_whitespace()
                    .map(|word| Token::Word(word.to_lowercase()))
                    .collect(),
            );
        }
        let mut tokens = Vec::new();
        for (index, part) in query.split('"').enumerate() {
            if index % 2 == 0 {
                tokens.extend(
                    part.split_whitespace()
                        .map(|word| Token::Word(word.to_lowercase())),
                );
            } else {
                let phrase = normalize_whitespace(&part.to_lowercase());
                if !phrase.is_empty() {
                    tokens.push(Token::Phrase(phrase));
                }
            }
        }
        Self(tokens)
    }

    pub fn matches(&self, session: &SessionSearch) -> bool {
        let fields = [&session.name, &session.cwd, &session.id, &session.preview];
        self.0.iter().all(|token| match token {
            Token::Word(word) => {
                fields.iter().any(|field| field.contains(word))
                    || fuzzy(word, &session.name)
                    || fuzzy(word, &session.cwd)
            }
            Token::Phrase(phrase) => fields
                .iter()
                .any(|field| normalize_whitespace(field).contains(phrase)),
        })
    }
}

fn normalize_whitespace(text: &str) -> String {
    let mut words = text.split_whitespace();
    let mut normalized = words.next().unwrap_or_default().to_owned();
    for word in words {
        normalized.push(' ');
        normalized.push_str(word);
    }
    normalized
}

fn subsequence(mut query: impl Iterator<Item = char>, text: &str) -> bool {
    let mut text = text.chars();
    query.all(|wanted| text.by_ref().any(|ch| ch == wanted))
}

fn fuzzy(query: &str, text: &str) -> bool {
    if subsequence(query.chars(), text) {
        return true;
    }
    // Pi also accepts a swapped ASCII letter/number suffix, e.g. api4 in
    // "4 API". Only names and folders get fuzzy matching; IDs/previews do not.
    let Some(split) = query.find(|ch: char| ch.is_ascii_digit()) else {
        return false;
    };
    let (letters, digits) = query.split_at(split);
    if !letters.is_empty()
        && letters.bytes().all(|ch| ch.is_ascii_alphabetic())
        && digits.bytes().all(|ch| ch.is_ascii_digit())
    {
        return subsequence(digits.chars().chain(letters.chars()), text);
    }
    let Some(split) = query.find(|ch: char| !ch.is_ascii_digit()) else {
        return false;
    };
    let (digits, letters) = query.split_at(split);
    !digits.is_empty()
        && digits.bytes().all(|ch| ch.is_ascii_digit())
        && letters.bytes().all(|ch| ch.is_ascii_alphabetic())
        && subsequence(letters.chars().chain(digits.chars()), text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{ProviderReplay, Role, ToolCall, ToolResult};
    use serde_json::json;

    #[test]
    fn message_queries_match_complete_text_parts_and_metadata_without_name_fuzziness() {
        let message = Message {
            role: Role::User,
            content: vec![
                Content::Text(format!("{}\nRésumé TARGET_α", "heading ".repeat(40))),
                Content::Text("across\r\n  lines and a literal \"quote".into()),
                Content::Image(serde_json::from_value(json!({
                    "mime_type": "image/png",
                    "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
                })).unwrap()),
            ],
            provider_replay: None,
        };
        let search = MessageSearch::new(&message, "Turn 17 ended completed");
        for query in [
            "résumé target_Α",
            "target_α turn 17",
            "image",
            "\"across lines\" résumé",
            "\"quote",
            "",
        ] {
            assert!(search.matches(&SessionQuery::new(query)), "{query}");
        }
        for query in [
            "absent",
            "résumé absent",
            "rsmtrgt",
            "\"target_α across\"",
            "ivborw0kggo",
        ] {
            assert!(!search.matches(&SessionQuery::new(query)), "{query}");
        }
    }

    #[test]
    fn message_search_does_not_index_reasoning_replay_or_tool_payloads() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                Content::Thinking("hidden_reasoning".into()),
                Content::Text("Visible answer".into()),
                Content::ToolCall(ToolCall {
                    id: "call".into(),
                    name: "visible_tool".into(),
                    arguments: json!({"value": "hidden_argument"}),
                    raw_arguments: None,
                }),
            ],
            provider_replay: Some(ProviderReplay::new(
                "fixture",
                "thinking",
                json!("opaque_replay"),
            )),
        };
        let search = MessageSearch::new(&message, "entry 4 continue after");
        assert!(search.matches(&SessionQuery::new("visible_tool answer entry 4")));
        for query in ["hidden_reasoning", "hidden_argument", "opaque_replay"] {
            assert!(!search.matches(&SessionQuery::new(query)), "{query}");
        }
        let result = Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "call".into(),
                name: "visible_tool".into(),
                result: json!({"value": "hidden_result"}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        };
        let search = MessageSearch::new(&result, "entry 5 after tool group");
        assert!(search.matches(&SessionQuery::new("visible_tool")));
        assert!(!search.matches(&SessionQuery::new("hidden_result")));
    }
}
