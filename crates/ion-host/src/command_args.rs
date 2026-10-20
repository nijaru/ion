//! One literal argv parser for prompt templates and explicit user commands.
//! Quotes and escapes group argv; there is no variable, glob or command
//! substitution. Expansion and execution remain separate owners.

use anyhow::{Result, anyhow};

/// Tokenize literal user arguments. This never consults a shell or filesystem.
pub fn parse_args(input: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (Some(open), close) if open == close => quote = None,
            (None, ch) if ch.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (Some('\''), ch) => {
                current.push(ch);
                started = true;
            }
            (_, '\\') => {
                current.push(
                    chars
                        .next()
                        .ok_or_else(|| anyhow!("trailing escape in arguments"))?,
                );
                started = true;
            }
            (_, ch) => {
                current.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(anyhow!("unclosed quote in arguments"));
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

/// Quote one argv value so a POSIX shell passes it through unchanged.
pub(crate) fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_literal_quotes_and_escapes_without_expansion() {
        assert_eq!(
            parse_args(r#"one "two words" 'three $HOME' four\ five"#).unwrap(),
            ["one", "two words", "three $HOME", "four five"]
        );
        assert_eq!(parse_args("\"; rm -rf nope\"").unwrap(), ["; rm -rf nope"]);
        assert!(parse_args("'unclosed").is_err());
        assert!(parse_args("trailing\\").is_err());
    }

    #[test]
    fn quotes_every_argv_value_for_one_shell_launch() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("'; echo nope; '"), "''\\''; echo nope; '\\'''");
        assert_eq!(quote("$()\n*?~"), "'$()\n*?~'");
    }
}
