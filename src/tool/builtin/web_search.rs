use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::Client;
use scraper::{Html, Selector};
use serde_json::json;
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 512 * 1024;

pub struct WebSearchTool {
    client: Client,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSearchTool {
    #[must_use]
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/120.0.0.0 Safari/537.36",
            )
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }
}

#[derive(Debug)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Extract the real URL from a DuckDuckGo link.
///
/// DDG may wrap result links in redirects like
/// `//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=...`,
/// or may serve direct URLs. Handles both.
fn extract_ddg_url(href: &str) -> Option<String> {
    let full = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    };

    let parsed = reqwest::Url::parse(&full).ok()?;

    if parsed.host_str() == Some("duckduckgo.com") {
        // DDG redirect — extract the uddg parameter
        for (key, value) in parsed.query_pairs() {
            if key == "uddg" {
                return Some(value.into_owned());
            }
        }
        return None;
    }

    Some(full)
}

/// Escape markdown link-breaking characters in a title.
fn escape_md_link_title(s: &str) -> String {
    s.replace('[', "\\[").replace(']', "\\]")
}

/// Escape closing parens in URLs for markdown link syntax.
fn escape_md_link_url(s: &str) -> String {
    s.replace(')', "%29")
}

/// Check if the response HTML is a DDG CAPTCHA/bot-detection page.
fn is_captcha_page(html: &str) -> bool {
    html.contains("anomaly-modal") || html.contains("Please distribute the ducks")
}

fn parse_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(html);

    // Use .web-result to exclude ad results (.result--ad)
    let result_sel = Selector::parse(".web-result").expect("valid selector");
    let title_sel = Selector::parse(".result__a").expect("valid selector");
    let snippet_sel = Selector::parse(".result__snippet").expect("valid selector");

    let mut results = Vec::new();

    for element in document.select(&result_sel) {
        if results.len() >= max_results {
            break;
        }

        let Some(title_el) = element.select(&title_sel).next() else {
            continue;
        };

        let title: String = title_el.text().collect::<String>().trim().to_string();
        if title.is_empty() {
            continue;
        }

        let url = title_el
            .value()
            .attr("href")
            .and_then(extract_ddg_url)
            .unwrap_or_default();

        if url.is_empty() {
            continue;
        }

        let snippet = element
            .select(&snippet_sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    results
}

/// Read the response body with a size cap, returning the text content.
async fn read_body_capped(response: reqwest::Response) -> Result<String, ToolError> {
    let mut bytes = Vec::with_capacity(MAX_RESPONSE_BYTES.min(64 * 1024));
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read response: {e}")))?;
        let remaining = MAX_RESPONSE_BYTES.saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        let take = chunk.len().min(remaining);
        bytes.extend_from_slice(&chunk[..take]);
    }

    String::from_utf8(bytes)
        .map_err(|e| ToolError::ExecutionFailed(format!("Response is not valid UTF-8: {e}")))
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo. Returns titles, URLs, and snippets."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 8, max: 20)"
                }
            },
            "required": ["query"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Restricted
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("query is required".to_string()))?;

        if query.trim().is_empty() {
            return Err(ToolError::InvalidArgs(
                "query must not be empty".to_string(),
            ));
        }

        #[allow(clippy::cast_possible_truncation)]
        let max_results = args
            .get("max_results")
            .and_then(serde_json::Value::as_u64)
            .map_or(8, |v| v.clamp(1, 20) as usize);

        let response = self
            .client
            .post("https://html.duckduckgo.com/html/")
            .form(&[("q", query)])
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Search request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult {
                content: format!("Search failed: HTTP {}", status.as_u16()),
                is_error: true,
                metadata: None,
            });
        }

        let html = read_body_capped(response).await?;

        if is_captcha_page(&html) {
            return Ok(ToolResult {
                content: "Search blocked: DuckDuckGo returned a CAPTCHA. Too many requests — wait a moment and try again.".to_string(),
                is_error: true,
                metadata: Some(json!({ "rate_limited": true })),
            });
        }

        let results = parse_results(&html, max_results);

        let mut output = format!("## Web Search: \"{query}\"\n\n");
        for (i, r) in results.iter().enumerate() {
            output.push_str(&format!(
                "{}. [{}]({})\n",
                i + 1,
                escape_md_link_title(&r.title),
                escape_md_link_url(&r.url),
            ));
            if !r.snippet.is_empty() {
                output.push_str(&format!("   {}\n", r.snippet));
            }
            output.push('\n');
        }
        output.push_str(&format!("({} results)", results.len()));

        Ok(ToolResult {
            content: output,
            is_error: false,
            metadata: Some(json!({
                "query": query,
                "result_count": results.len(),
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Snapshot with DDG redirect URLs (legacy format, still handled)
    const DDG_HTML_REDIRECT: &str = r#"
    <div class="results">
        <div class="result results_links results_links_deep web-result">
            <div class="links_main links_deep result__body">
                <h2 class="result__title">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2Fch16-00-concurrency.html&rut=abc">
                        Fearless Concurrency - The Rust Programming Language
                    </a>
                </h2>
                <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2Fch16-00-concurrency.html&rut=abc">
                    Handling Concurrent Programming Safely and Efficiently
                </a>
            </div>
        </div>
        <div class="result results_links results_links_deep web-result">
            <div class="links_main links_deep result__body">
                <h2 class="result__title">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftokio.rs%2Ftokio%2Ftutorial&rut=def">
                        Tokio Tutorial
                    </a>
                </h2>
                <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftokio.rs%2Ftokio%2Ftutorial&rut=def">
                    An introduction to asynchronous programming in Rust with Tokio
                </a>
            </div>
        </div>
    </div>
    "#;

    // Snapshot with direct URLs (current DDG format)
    const DDG_HTML_DIRECT: &str = r#"
    <div class="results">
        <div class="result results_links results_links_deep web-result">
            <div class="links_main links_deep result__body">
                <h2 class="result__title">
                    <a class="result__a" href="https://doc.rust-lang.org/book/ch16-00-concurrency.html">
                        Fearless Concurrency - The Rust Programming Language
                    </a>
                </h2>
                <a class="result__snippet" href="https://doc.rust-lang.org/book/ch16-00-concurrency.html">
                    Handling Concurrent Programming Safely and Efficiently
                </a>
            </div>
        </div>
        <div class="result results_links results_links_deep web-result">
            <div class="links_main links_deep result__body">
                <h2 class="result__title">
                    <a class="result__a" href="https://en.wikipedia.org/wiki/Rust_(programming_language)">
                        Rust (programming language) - Wikipedia
                    </a>
                </h2>
                <a class="result__snippet" href="https://en.wikipedia.org/wiki/Rust_(programming_language)">
                    Rust is a multi-paradigm, general-purpose programming language
                </a>
            </div>
        </div>
    </div>
    "#;

    // Snapshot with ad results mixed in (should be filtered)
    const DDG_HTML_WITH_ADS: &str = r#"
    <div class="results">
        <div class="result result--ad">
            <div class="result__body">
                <h2 class="result__title">
                    <a class="result__a" href="//duckduckgo.com/y.js?ad_provider=bingv7aa&rurl=https%3A%2F%2Fwww.example-ad.com">
                        Sponsored Result
                    </a>
                </h2>
                <a class="result__snippet">Buy our stuff!</a>
            </div>
        </div>
        <div class="result results_links results_links_deep web-result">
            <div class="links_main links_deep result__body">
                <h2 class="result__title">
                    <a class="result__a" href="https://example.com/real-result">
                        Real Organic Result
                    </a>
                </h2>
                <a class="result__snippet">Actual search result content</a>
            </div>
        </div>
    </div>
    "#;

    const DDG_CAPTCHA_HTML: &str = r#"
    <html>
    <body>
        <div class="anomaly-modal">
            <div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
            <div class="anomaly-modal__description">Please complete the CAPTCHA below.</div>
        </div>
    </body>
    </html>
    "#;

    #[test]
    fn test_parse_redirect_urls() {
        let results = parse_results(DDG_HTML_REDIRECT, 10);
        assert_eq!(results.len(), 2);

        assert_eq!(
            results[0].title,
            "Fearless Concurrency - The Rust Programming Language"
        );
        assert_eq!(
            results[0].url,
            "https://doc.rust-lang.org/book/ch16-00-concurrency.html"
        );
        assert_eq!(
            results[0].snippet,
            "Handling Concurrent Programming Safely and Efficiently"
        );

        assert_eq!(results[1].title, "Tokio Tutorial");
        assert_eq!(results[1].url, "https://tokio.rs/tokio/tutorial");
    }

    #[test]
    fn test_parse_direct_urls() {
        let results = parse_results(DDG_HTML_DIRECT, 10);
        assert_eq!(results.len(), 2);

        assert_eq!(
            results[0].url,
            "https://doc.rust-lang.org/book/ch16-00-concurrency.html"
        );
        assert_eq!(results[1].title, "Rust (programming language) - Wikipedia");
        assert_eq!(
            results[1].url,
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
    }

    #[test]
    fn test_ads_filtered() {
        let results = parse_results(DDG_HTML_WITH_ADS, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Real Organic Result");
    }

    #[test]
    fn test_captcha_detection() {
        assert!(is_captcha_page(DDG_CAPTCHA_HTML));
        assert!(!is_captcha_page(DDG_HTML_DIRECT));
        assert!(!is_captcha_page("<html><body>normal</body></html>"));
    }

    #[test]
    fn test_url_extraction() {
        // DDG redirect URL
        assert_eq!(
            extract_ddg_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc"),
            Some("https://example.com/page".to_string())
        );

        // Direct URL
        assert_eq!(
            extract_ddg_url("https://example.com/direct"),
            Some("https://example.com/direct".to_string())
        );

        // DDG redirect without uddg param (ad click tracker)
        assert_eq!(
            extract_ddg_url("//duckduckgo.com/y.js?ad_provider=bing"),
            None
        );

        // Invalid URL
        assert_eq!(extract_ddg_url("not a url at all"), None);
    }

    #[test]
    fn test_max_results_clamping() {
        let results = parse_results(DDG_HTML_REDIRECT, 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_empty_html() {
        let results = parse_results("<html><body></body></html>", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_markdown_escaping() {
        assert_eq!(
            escape_md_link_title("Rust (programming language) [official]"),
            "Rust (programming language) \\[official\\]"
        );
        assert_eq!(
            escape_md_link_url("https://en.wikipedia.org/wiki/Rust_(language)"),
            "https://en.wikipedia.org/wiki/Rust_(language%29"
        );
    }

    #[tokio::test]
    async fn test_empty_query() {
        let tool = WebSearchTool::new();
        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_id: "test".into(),
            abort_signal: tokio_util::sync::CancellationToken::new(),
            no_sandbox: false,
            index_callback: None,
        };

        let result = tool.execute(json!({"query": ""}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));

        let result = tool.execute(json!({"query": "   "}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));

        let result = tool.execute(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }
}
