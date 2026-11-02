//! No executable content or remote subresources: all saved images are typed data URLs.
use std::fmt::Write as _;

use ion_ai::ImageContent;
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd};

use super::Document;

pub(super) struct Html(String);

impl Html {
    pub(super) fn new() -> Self {
        Self(
            concat!(
                "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">",
                "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
                "<meta name=\"referrer\" content=\"no-referrer\">",
                "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; ",
                "script-src 'none'; style-src 'unsafe-inline'; img-src data:; ",
                "base-uri 'none'; form-action 'none'; object-src 'none'\">",
                "<title>Ion transcript</title><style>",
                include_str!("style.css"),
                "</style></head><body><main><h1>Ion transcript</h1>",
                "<p class=\"notice\">Committed selected-path snapshot, not a Session backup. ",
                "Includes prompts, thinking, tool output and saved images. Review before sharing. ",
                "Other branches are omitted; selecting context does not restore files.</p>"
            )
            .into(),
        )
    }

    fn element(&mut self, tag: &str, value: &str) {
        let _ = writeln!(self.0, "<{tag}>{}</{tag}>", escape(value));
    }
}

impl Document for Html {
    fn metadata(&mut self, label: &str, value: &str) {
        let _ = writeln!(
            self.0,
            "<p><strong>{}:</strong> {}</p>",
            escape(label),
            escape(value)
        );
    }
    fn heading(&mut self, value: &str) {
        self.element("h2", value);
    }
    fn label(&mut self, value: &str) {
        self.element("h3", value);
    }
    fn literal(&mut self, value: &str) {
        // A code child prevents HTML from discarding a leading newline in pre.
        self.0.push_str("<pre><code>");
        self.0.push_str(&escape(value));
        self.0.push_str("</code></pre>\n");
    }
    fn markdown(&mut self, value: &str) {
        // Parse only sanitized text; original HTML tokens become literal text. Static
        // span tokens below are ours, never model-supplied HTML. Keep the literal
        // source too, including rejected URLs and Markdown image destinations.
        let source = controls(value);
        let mut links = Vec::new();
        let mut images: Vec<CowStr<'_>> = Vec::new();
        let events = Parser::new_ext(
            &source,
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
        )
        .map(|event| match event {
            Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let url = link_url(&dest_url);
                links.push(url.is_some());
                if let Some(url) = url {
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url: url.into(),
                        title,
                        id,
                    })
                } else {
                    Event::Html("<span>".into())
                }
            }
            Event::End(TagEnd::Link) => {
                if links.pop().expect("parser balances links") {
                    Event::End(TagEnd::Link)
                } else {
                    Event::Html("</span>".into())
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                images.push(dest_url);
                Event::Text("[image: ".into())
            }
            Event::End(TagEnd::Image) => {
                Event::Text(format!("] ({})", images.pop().expect("parser balances images")).into())
            }
            event => event,
        });
        self.0.push_str("<div class=\"markdown\">");
        pulldown_cmark::html::push_html(&mut self.0, events);
        self.0
            .push_str("</div><details><summary>Markdown source</summary>");
        self.literal(value);
        self.0.push_str("</details>\n");
    }
    fn thinking(&mut self, value: &str) {
        self.0.push_str("<details><summary>Thinking</summary>");
        self.literal(value);
        self.0.push_str("</details>\n");
    }
    fn image(&mut self, image: &ImageContent) {
        let mime = image.mime_type().as_str();
        // ImageContent admits only validated PNG/JPEG/GIF/WebP base64, not SVG or HTML.
        let _ = writeln!(
            self.0,
            "<figure><img alt=\"Saved {mime} image\" src=\"data:{mime};base64,{}\"><figcaption>{mime}</figcaption></figure>",
            image.data()
        );
    }
    fn finish(mut self) -> String {
        self.0.push_str("</main></body></html>\n");
        self.0
    }
}

fn controls(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch == '\n' || ch == '\t' || !ch.is_control() {
                ch
            } else {
                '�'
            }
        })
        .collect()
}

fn escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            ch if ch != '\n' && ch != '\t' && ch.is_control() => escaped.push('�'),
            ch => escaped.push(ch),
        }
    }
    escaped
}

/// Like the Pi exporter, strip browser-ignored controls before checking schemes.
/// Relative links are user navigation, never automatically loaded subresources.
fn link_url(value: &str) -> Option<String> {
    let value: String = value.chars().filter(|ch| !ch.is_control()).collect();
    let value = value.trim();
    if let Some((scheme, _)) = value.split_once(':')
        && !scheme.is_empty()
        && scheme.chars().enumerate().all(|(index, ch)| {
            ch.is_ascii_alphabetic() || (index > 0 && (ch.is_ascii_digit() || "+.-".contains(ch)))
        })
        && !["http", "https", "mailto", "tel", "ftp"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    {
        return None;
    }
    Some(value.into())
}
