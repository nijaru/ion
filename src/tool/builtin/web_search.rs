use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use reqwest::Client;
use scraper::{Html, Selector};
use serde_json::json;
use std::time::Duration;

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

/// Extract the real URL from a DuckDuckGo redirect link.
///
/// DDG wraps result links like `//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=...`.
/// We extract the `uddg` parameter and URL-decode it.
fn extract_ddg_url(href: &str) -> Option<String> {
    // Try to parse as a URL (may be protocol-relative)
    let full = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    };

    if let Ok(parsed) = reqwest::Url::parse(&full) {
        // If it's a DDG redirect, extract the uddg parameter
        if parsed.host_str() == Some("duckduckgo.com") {
            for (key, value) in parsed.query_pairs() {
                if key == "uddg" {
                    return Some(value.into_owned());
                }
            }
            return None;
        }
        // Already a direct URL
        return Some(full);
    }

    None
}

fn parse_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(html);

    let result_sel = Selector::parse(".result").expect("valid selector");
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
            return Err(ToolError::InvalidArgs("query must not be empty".to_string()));
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

        let html = response
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read response: {e}")))?;

        let results = parse_results(&html, max_results);

        let mut output = format!("## Web Search: \"{query}\"\n\n");
        for (i, r) in results.iter().enumerate() {
            output.push_str(&format!("{}. [{}]({})\n", i + 1, r.title, r.url));
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

    const DDG_HTML_SNAPSHOT: &str = r#"
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
        <div class="result results_links results_links_deep web-result">
            <div class="links_main links_deep result__body">
                <h2 class="result__title">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fblog.example.com%2Fasync-rust&rut=ghi">
                        Understanding Async Rust
                    </a>
                </h2>
            </div>
        </div>
    </div>
    "#;

    #[test]
    fn test_parse_results() {
        let results = parse_results(DDG_HTML_SNAPSHOT, 10);
        assert_eq!(results.len(), 3);

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

        // Third result has no snippet
        assert_eq!(results[2].title, "Understanding Async Rust");
        assert!(results[2].snippet.is_empty());
    }

    #[test]
    fn test_url_extraction() {
        // DDG redirect URL
        assert_eq!(
            extract_ddg_url(
                "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc"
            ),
            Some("https://example.com/page".to_string())
        );

        // Direct URL
        assert_eq!(
            extract_ddg_url("https://example.com/direct"),
            Some("https://example.com/direct".to_string())
        );

        // DDG redirect without uddg param
        assert_eq!(extract_ddg_url("//duckduckgo.com/l/?other=value"), None);

        // Invalid URL
        assert_eq!(extract_ddg_url("not a url at all"), None);
    }

    #[test]
    fn test_max_results_clamping() {
        let results = parse_results(DDG_HTML_SNAPSHOT, 2);
        assert_eq!(results.len(), 2);

        let results = parse_results(DDG_HTML_SNAPSHOT, 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_empty_html() {
        let results = parse_results("<html><body></body></html>", 10);
        assert!(results.is_empty());
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
            discovery_callback: None,
        };

        let result = tool.execute(json!({"query": ""}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));

        let result = tool.execute(json!({"query": "   "}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));

        let result = tool.execute(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }
}
