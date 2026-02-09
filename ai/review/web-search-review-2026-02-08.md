# Web Search Tool Review

**Date:** 2026-02-08
**Files:** `src/tool/builtin/web_search.rs`, `src/tool/builtin/mod.rs`, `src/tool/mod.rs`, `Cargo.toml`
**Status:** Tests pass (5/5), clippy clean

## Critical Issues (must fix)

### 1. No CAPTCHA/bot detection handling (web_search.rs:180-191)

**Confidence: 95%**

DDG returns a CAPTCHA challenge page (`anomaly-modal`) instead of results when it detects automated traffic. The current code treats this as a successful response (HTTP 200), parses 0 results, and returns `(0 results)` with no indication that results were blocked.

Verified live: first request succeeded, second request same session got CAPTCHA'd.

**Impact:** Tool silently returns no results, LLM has no idea why.

**Fix:** After parsing, if `results.is_empty()` and the HTML contains `anomaly-modal` or similar markers, return an error message like "Search blocked by CAPTCHA. Try again later or reduce request frequency."

```rust
// After parse_results()
if results.is_empty() {
    if html.contains("anomaly-modal") || html.contains("bot") {
        return Ok(ToolResult {
            content: "Search blocked: DuckDuckGo returned a CAPTCHA challenge. Try again later.".into(),
            is_error: true,
            metadata: None,
        });
    }
}
```

### 2. No response body size limit (web_search.rs:188-191)

**Confidence: 90%**

`response.text().await` reads the entire response body into memory with no upper bound. While DDG responses are typically ~40KB, a misbehaving proxy, redirect, or future DDG change could return an arbitrarily large response.

Compare with `web_fetch.rs:208-222` which streams with a configurable size limit.

**Fix:** Either use streaming with a cap (e.g., 512KB is more than enough for DDG HTML), or at minimum check `Content-Length` header before reading:

```rust
let content_length = response.content_length().unwrap_or(0);
if content_length > 512_000 {
    return Ok(ToolResult {
        content: "Search response too large".into(),
        is_error: true,
        metadata: None,
    });
}
```

## Important Issues (should fix)

### 3. Markdown injection via titles containing brackets/parens (web_search.rs:197)

**Confidence: 85%**

The output format `[{title}]({url})` will produce broken markdown if the title contains `[` or `]` characters, or if the URL contains `)`. Real DDG results can have titles like `Rust (programming language)` which would break the link syntax.

**Fix:** Escape brackets in title, encode parens in URL, or just use a non-markdown format:

```rust
// Option A: Plain text format (simplest, most robust)
output.push_str(&format!("{}. {}\n   {}\n", i + 1, r.title, r.url));

// Option B: Escape markdown special chars
let safe_title = r.title.replace('[', "\\[").replace(']', "\\]");
let safe_url = r.url.replace(')', "%29");
```

### 4. Test fixture uses outdated DDG HTML format (web_search.rs:220-256)

**Confidence: 95%**

The test fixture uses `//duckduckgo.com/l/?uddg=...` redirect URLs, but live DDG now returns direct URLs (e.g., `href="https://rust-lang.org/"`). The code handles both formats correctly, but the test doesn't cover the current real-world format.

**Fix:** Add a test with direct URLs matching current DDG behavior:

```rust
#[test]
fn test_parse_direct_urls() {
    let html = r#"
    <div class="result results_links web-result">
        <h2 class="result__title">
            <a class="result__a" href="https://rust-lang.org/">
                Rust Programming Language
            </a>
        </h2>
        <a class="result__snippet" href="https://rust-lang.org/">
            A language empowering everyone to build reliable software.
        </a>
    </div>
    "#;
    let results = parse_results(html, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://rust-lang.org/");
}
```

### 5. Ads not explicitly filtered (web_search.rs:80)

**Confidence: 80%**

The `.result` selector matches ad results (`result--ad` class). Currently ads are implicitly filtered because their URLs (`duckduckgo.com/y.js?...`) don't have a `uddg` param, so `extract_ddg_url` returns `None`. This is fragile -- if DDG changes ad URL format to include `uddg`, ads would leak through.

**Fix:** Use `.result.web-result` selector or explicitly exclude `.result--ad`:

```rust
let result_sel = Selector::parse(".result.web-result").expect("valid selector");
// OR
let result_sel = Selector::parse(".result:not(.result--ad)").expect("valid selector");
```

### 6. Inconsistent danger_level classification (web_search.rs:148)

**Confidence: 70%**

`web_search` is marked `DangerLevel::Restricted`, matching `web_fetch`. This is reasonable since it makes network requests. However, the search query is user-text-only (no URL/path control), so it's arguably safer than `web_fetch`. Not a bug, but worth noting the classification is conservative. In Read mode, agents won't have web search which may be unexpected.

## Minor Issues (nice to have)

### 7. Selector::parse called on every invocation (web_search.rs:74-76)

**Confidence: 75%**

`Selector::parse` is called every time `parse_results` runs. These are constant strings that could be parsed once. Currently not a performance concern (they're fast), but it's a minor inefficiency.

**Fix:** Use `std::sync::LazyLock` or `once_cell::sync::Lazy` for static selectors.

### 8. No query length validation (web_search.rs:161-163)

**Confidence: 60%**

The only validation is non-empty. Very long queries (thousands of chars) would be sent to DDG as-is. DDG likely truncates, but sending huge form bodies is wasteful.

**Fix:** Cap query length (e.g., 500 chars).

### 9. `scraper` dependency adds weight (Cargo.toml:60)

**Confidence: 50%**

The `scraper` crate pulls in `html5ever`, `selectors`, `cssparser`, etc. This is a significant dependency tree for parsing a simple HTML structure. An alternative would be regex-based extraction (fragile) or using the already-present `html2text` crate from `web_fetch`. Not actionable unless binary size is a concern.

## Pattern Consistency with web_fetch.rs

| Aspect                | web_fetch       | web_search      | Notes                                     |
| --------------------- | --------------- | --------------- | ----------------------------------------- |
| Client timeout        | 30s             | 15s             | Search is OK shorter                      |
| User-Agent            | `ion/0.0.0`     | Chrome UA       | Search needs browser UA to avoid blocking |
| SSRF protection       | Yes             | N/A (fixed URL) | Correct                                   |
| Response size limit   | Streaming + cap | None            | **Gap**                                   |
| Error on HTTP failure | is_error: true  | is_error: true  | Consistent                                |
| Metadata in result    | Yes (rich)      | Yes (basic)     | Fine                                      |
| `_ctx` unused         | Yes             | Yes             | Consistent                                |

## Summary

The implementation is well-structured, correctly handles both DDG URL formats, and has good test coverage for parsing logic. The two critical issues are the silent failure on CAPTCHA and unbounded response reading. The markdown injection and ad filtering are important correctness fixes. Everything else is polish.

Priority order: #1 CAPTCHA detection > #2 response size cap > #3 markdown injection > #5 ad filter selector > #4 test fixture > rest.
