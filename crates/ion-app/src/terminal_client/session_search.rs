//! Search captured Session fields, never their painted labels or live files.
use ion_host::SessionSummary;

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
