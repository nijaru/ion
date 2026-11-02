//! Streaming text projection, not DOM reconstruction or layout. Formatting
//! reconstruction can clone attributes quadratically even on small HTML input.
use super::content::{Failure, check};
use html5ever::{
    tendril::StrTendril,
    tokenizer::{
        BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
        states::RawKind,
    },
};
use std::cell::RefCell;
use tokio_util::sync::CancellationToken;
use url::Url;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TAGS: usize = 50_000;
const MAX_PENDING_BYTES: usize = 64 * 1024;
const IGNORED: [&str; 8] = [
    "script", "style", "template", "svg", "canvas", "iframe", "noembed", "noframes",
];

pub(super) fn extract(
    source: &str,
    base: Url,
    stop: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<String, Failure> {
    // Resolve document base before rendering earlier links. Both passes stream
    // tokens, including hidden/template contents, without ever building a DOM.
    let base = tokenize(source, State::new(base, false), stop, deadline)?.base;
    let mut rendered = tokenize(source, State::new(base, true), stop, deadline)?;
    rendered.close_link()?;
    Ok(rendered.output.value)
}

fn tokenize(
    source: &str,
    state: State,
    stop: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<State, Failure> {
    let tokenizer = Tokenizer::new(
        Sink {
            state: RefCell::new(state),
            stop,
            deadline,
        },
        TokenizerOpts::default(),
    );
    let input = BufferQueue::default();
    let mut start = 0;
    let mut pending_bytes = 0;
    while start < source.len() {
        check(stop, deadline)?;
        let mut end = (start + 4096).min(source.len());
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        let before = tokenizer.sink.state.borrow().tokens;
        input.push_back(StrTendril::from_slice(&source[start..end]));
        let _ = tokenizer.feed(&input);
        let state = tokenizer.sink.state.borrow();
        if let Some(error) = state.failure {
            return Err(error);
        }
        if state.tokens == before {
            pending_bytes += end - start;
        } else {
            pending_bytes = 0;
        }
        if pending_bytes > MAX_PENDING_BYTES {
            return Err((
                "conversion_limit",
                "HTML token exceeds the 64 KiB parse limit.",
            ));
        }
        start = end;
    }
    tokenizer.end();
    let state = tokenizer.sink.state.into_inner();
    if let Some(error) = state.failure {
        return Err(error);
    }
    check(stop, deadline)?;
    Ok(state)
}

struct Sink<'a> {
    state: RefCell<State>,
    stop: &'a CancellationToken,
    deadline: tokio::time::Instant,
}
impl TokenSink for Sink<'_> {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mode = match &token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => match &*tag.name {
                "script" => TokenSinkResult::RawData(RawKind::ScriptData),
                "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                    TokenSinkResult::RawData(RawKind::Rawtext)
                }
                "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
                "plaintext" => TokenSinkResult::Plaintext,
                _ => TokenSinkResult::Continue,
            },
            _ => TokenSinkResult::Continue,
        };
        let mut state = self.state.borrow_mut();
        if state.failure.is_some() {
            return mode;
        }
        // Diagnostics can be emitted while a tag/attribute is still buffered;
        // they do not release that allocation or reset the pending-token budget.
        if !matches!(token, Token::ParseError(_)) {
            state.tokens += 1;
        }
        let result = check(self.stop, self.deadline).and_then(|()| match token {
            Token::TagToken(tag) => state.tag(tag),
            Token::CharacterTokens(text) if state.render && state.ignored.is_none() => {
                let pre = state.pre > 0;
                state.output.text(&text, pre)
            }
            _ => Ok(()),
        });
        if let Err(error) = result {
            state.failure = Some(error);
        }
        mode
    }
}

struct State {
    base: Url,
    found_base: bool,
    render: bool,
    output: Text,
    ignored: Option<(usize, usize)>,
    pre: usize,
    link: Option<Url>,
    tags: usize,
    tokens: usize,
    failure: Option<Failure>,
}
impl State {
    fn new(base: Url, render: bool) -> Self {
        Self {
            base,
            found_base: false,
            render,
            output: Text::default(),
            ignored: None,
            pre: 0,
            link: None,
            tags: 0,
            tokens: 0,
            failure: None,
        }
    }
    fn tag(&mut self, tag: Tag) -> Result<(), Failure> {
        let name: &str = &tag.name;
        let closing = tag.kind == TagKind::EndTag;
        if !closing {
            self.tags += 1;
        }
        if self.tags > MAX_TAGS || tag.attrs.len() > 256 {
            return Err((
                "conversion_limit",
                "HTML exceeds the 50000-tag/256-attributes-per-tag limit.",
            ));
        }
        // One opaque skipped scope. Tags inside an inert template must neither
        // change the document base nor leave unrelated skip flags behind.
        if let Some((index, depth)) = self.ignored {
            if name == IGNORED[index] {
                if closing {
                    self.ignored = (depth > 1).then_some((index, depth.saturating_sub(1)));
                } else if !(name == "svg" && tag.self_closing) {
                    self.ignored = Some((index, depth + 1));
                }
            }
            return Ok(());
        }
        if let Some(index) = IGNORED.iter().position(|v| *v == name) {
            if !closing && !(name == "svg" && tag.self_closing) {
                self.ignored = Some((index, 1));
            }
            return Ok(());
        }
        if !self.render {
            if !closing
                && name == "base"
                && !self.found_base
                && let Some(href) = tag.attrs.iter().find(|a| &*a.name.local == "href")
                && let Ok(url) = self.base.join(&href.value)
            {
                self.base = url;
                self.found_base = true;
            }
            return Ok(());
        }
        if closing {
            match name {
                "a" => self.close_link()?,
                "td" | "th" => self.output.push("\t")?,
                "pre" => {
                    self.pre = self.pre.saturating_sub(1);
                    self.output.newline()?;
                }
                _ if block(name) => self.output.newline()?,
                _ => {}
            }
            return Ok(());
        }
        if block(name) {
            self.output.newline()?;
        }
        match name {
            "pre" => self.pre += 1,
            "br" | "hr" => self.output.newline()?,
            "li" => self.output.push("* ")?,
            "a" => {
                self.close_link()?;
                if let Some(href) = tag.attrs.iter().find(|a| &*a.name.local == "href")
                    && let Ok(url) = self.base.join(&href.value)
                {
                    self.output.pending_space()?;
                    self.output.push("[")?;
                    self.link = Some(url);
                }
            }
            "img" => {
                if let Some(alt) = tag.attrs.iter().find(|a| &*a.name.local == "alt") {
                    self.output.text(&alt.value, false)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn close_link(&mut self) -> Result<(), Failure> {
        if let Some(url) = self.link.take() {
            let pending_space = self.output.space;
            self.output.push("](")?;
            self.output.push(url.as_str())?;
            self.output.push(")")?;
            self.output.space = pending_space;
        }
        Ok(())
    }
}

fn block(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "aside"
            | "nav"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "table"
            | "tr"
            | "pre"
            | "dl"
            | "dt"
            | "dd"
    )
}

#[derive(Default)]
struct Text {
    value: String,
    space: bool,
}
impl Text {
    fn push(&mut self, text: &str) -> Result<(), Failure> {
        if text.len() > MAX_TEXT_BYTES - self.value.len() {
            return Err((
                "conversion_limit",
                "Converted text exceeds the 4 MiB limit.",
            ));
        }
        self.value.push_str(text);
        self.space = false;
        Ok(())
    }
    fn newline(&mut self) -> Result<(), Failure> {
        if !self.value.is_empty() && !self.value.ends_with('\n') {
            self.push("\n")?;
        }
        self.space = false;
        Ok(())
    }
    fn pending_space(&mut self) -> Result<(), Failure> {
        if self.space && !self.value.is_empty() && !self.value.ends_with(['\n', '\t']) {
            self.push(" ")?;
        }
        self.space = false;
        Ok(())
    }
    fn text(&mut self, text: &str, pre: bool) -> Result<(), Failure> {
        if pre {
            return self.push(text);
        }
        for c in text.chars() {
            if c.is_whitespace() {
                self.space = true;
            } else {
                self.pending_space()?;
                self.push(c.encode_utf8(&mut [0; 4]))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_budget_rejects_append_before_growing_buffer() {
        let mut text = Text {
            value: "x".repeat(MAX_TEXT_BYTES - 1),
            space: false,
        };
        assert!(text.push("é").is_err());
        assert_eq!(text.value.len(), MAX_TEXT_BYTES - 1);
        text.push("x").unwrap();
        assert_eq!(text.value.len(), MAX_TEXT_BYTES);
    }
}
