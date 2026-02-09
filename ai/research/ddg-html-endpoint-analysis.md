# DuckDuckGo HTML Endpoint: Live Response Analysis

**Research Date**: 2026-02-08
**Purpose**: Verify ion's web_search implementation against real DDG HTML responses
**Source File**: `/Users/nick/github/nijaru/ion/src/tool/builtin/web_search.rs`

---

## 1. Selector Verification

Tested query: `q=rust+async+patterns` via POST to `https://html.duckduckgo.com/html/`

### 1a. `.result` selector -- CORRECT with caveats

Real HTML structure for organic results:

```html
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title">
      <a rel="nofollow" class="result__a" href="https://doc.rust-lang.org/..."
        >Title</a
      >
    </h2>
    <div class="result__extras">...</div>
    <a class="result__snippet" href="..."
      >Snippet text with <b>bold</b> terms</a
    >
    <div class="clear"></div>
  </div>
</div>
```

The `.result` class selector matches both organic results AND ad results. The full class list for organic results is `result results_links results_links_deep web-result`. Ad results use `result results_links results_links_deep result--ad` (see Section 3).

**Current implementation works** because it correctly matches on `.result` which encompasses both, but lacks ad filtering.

### 1b. `.result__a` selector -- CORRECT

The title link consistently uses `<a class="result__a">` inside an `<h2 class="result__title">`. This is stable across all test responses (organic, ads, different query types).

### 1c. `.result__snippet` selector -- CORRECT

Snippets consistently use `<a class="result__snippet">`. Note: the snippet is itself an `<a>` tag (a link), not a `<div>` or `<span>`. Snippets contain inline `<b>` tags for query term highlighting. The `.text().collect()` approach in the implementation correctly strips these to plain text.

### 1d. Test snapshot accuracy -- STALE

The test snapshot `DDG_HTML_SNAPSHOT` in the code differs from the real response in two important ways:

| Aspect             | Test Snapshot                                        | Real Response (2026-02-08)                       |
| ------------------ | ---------------------------------------------------- | ------------------------------------------------ |
| URL format in href | `//duckduckgo.com/l/?uddg=...` (redirect)            | Direct URLs like `https://doc.rust-lang.org/...` |
| Snippet element    | `<a class="result__snippet">`                        | `<a class="result__snippet">` (matches)          |
| Result class       | `result results_links results_links_deep web-result` | Same (matches)                                   |
| rel attribute      | Not present                                          | `rel="nofollow"` on links                        |

**The uddg redirect pattern in the test snapshot does not match current DDG behavior for organic results.** See Section 2 for details.

---

## 2. URL Wrapping Pattern -- CHANGED

### Current implementation assumption (OUTDATED for organic results)

The code assumes DDG wraps organic result URLs as `//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=...` and implements `extract_ddg_url()` to extract the `uddg` parameter.

### Actual behavior observed (2026-02-08)

**Organic results now use DIRECT URLs:**

```html
<a
  rel="nofollow"
  class="result__a"
  href="https://doc.rust-lang.org/book/ch17-00-async-await.html"
></a>
```

No DDG redirect wrapping. No `//duckduckgo.com/l/?uddg=` pattern. The `href` is the final destination URL directly.

Verified across 4 different queries with 40+ organic results total: all organic results had direct URLs.

**Ad results use `duckduckgo.com/y.js?` redirect (different pattern):**

```html
<a
  class="result__a"
  href="https://duckduckgo.com/y.js?ad_domain=top10.com&ad_provider=bingv7aa&ad_type=txad&...&u3=https%3A%2F%2Fwww.bing.com%2Faclick%3F..."
></a>
```

The ad redirect uses `/y.js?` path with an `ad_domain` parameter, not `/l/?uddg=`.

### Impact on `extract_ddg_url()`

The function handles this correctly by accident:

1. For direct URLs like `https://doc.rust-lang.org/...`, it falls through to the `Some(full)` return at line 66 (not a DDG host, so returns the URL directly)
2. For the old `//duckduckgo.com/l/?uddg=` pattern, it would still work (extracts `uddg` param)
3. For ad URLs via `duckduckgo.com/y.js?`, it fails correctly -- no `uddg` parameter exists, returns `None`, and the result is skipped

So the function works in practice, but the logic it was designed around (the `uddg` redirect pattern) is not what organic results actually look like anymore. The function is essentially a passthrough for direct URLs, with dead code for `uddg` extraction.

---

## 3. Ad Results -- CRITICAL FINDING

### Ad result structure

Ad results are present for commercial queries. They use the **same container class** `.result` but with additional classes:

```
result results_links results_links_deep result--ad
result results_links results_links_deep result--ad result--ad--small
```

Key differentiators:

| Feature         | Organic Result | Ad Result                                  |
| --------------- | -------------- | ------------------------------------------ |
| Container class | `web-result`   | `result--ad`                               |
| Badge element   | None           | `<button class="badge--ad">Ad</button>`    |
| URL in href     | Direct URL     | `duckduckgo.com/y.js?ad_domain=...&u3=...` |
| URL destination | Target site    | Bing aclick redirect chain                 |

### Current behavior: ads ARE included in results

Because the parser matches on `.result` and ads have `class="result ... result--ad"`, the `.result` selector matches them. The ads are only excluded because their `href` points to `duckduckgo.com/y.js?...` which lacks a `uddg` parameter, causing `extract_ddg_url()` to return `None`, which causes the result to be skipped (lines 97-101).

This is **fragile**. If DDG ever adds a `uddg` parameter to ad URLs, or changes the redirect mechanism, ads would leak into results.

### Recommendation

Filter explicitly on `result--ad`:

```rust
for element in document.select(&result_sel) {
    // Skip ad results
    if element.value().attr("class")
        .map_or(false, |c| c.contains("result--ad"))
    {
        continue;
    }
    // ... parse organic result
}
```

Or use a more specific selector: `.result.web-result` instead of just `.result`.

---

## 4. Zero-Click Info Boxes

Tested query `q=what+is+python` which should trigger an instant answer.

**Finding**: The HTML endpoint does NOT include zero-click info boxes. The response contains only standard web results in the `#links .results` container. The zero-click/instant answer functionality is exclusive to the JavaScript version (`duckduckgo.com`) and the Instant Answer API (`api.duckduckgo.com/?format=json`).

No additional selectors or handling needed.

---

## 5. Special Characters

Tested query: `c++ "template<T>" && <std>` (URL-encoded as `q=c%2B%2B+%22template%3CT%3E%22+%26%26+%3Cstd%3E`)

**Result**: DDG correctly handles URL-encoded special characters. reqwest's `.form(&[("q", query)])` correctly encodes form data, so no special handling is needed in the implementation. The response contained 10 valid organic results with direct URLs.

HTML entities in snippets (e.g., `&#x27;` for apostrophe, `&amp;` for ampersand) are correctly handled by the `scraper` crate's text extraction.

---

## 6. Rate Limiting and Blocking

### HTTP Status Codes Observed

| Status | Meaning                   | When Triggered                            |
| ------ | ------------------------- | ----------------------------------------- |
| `200`  | Success, results returned | Normal operation, up to ~4 rapid requests |
| `202`  | CAPTCHA/bot challenge     | After ~5 rapid requests from same IP      |

No `403` or `429` status codes were observed. The rate limit response is **HTTP 202**, not 403 or 429.

### CAPTCHA Page Structure

The 202 response is approximately 14KB and contains a "select all squares containing a duck" CAPTCHA. Key identifiers:

- Page title: `DuckDuckGo` (not the search query)
- Contains `class="anomaly-modal"` elements
- Contains text: "Unfortunately, bots use DuckDuckGo too."
- Form action points to `//duckduckgo.com/anomaly.js?...&cc=botnet`
- Contains `<img class="anomaly-modal__image">` elements (9 tiles)
- No `.result` or `.result__a` elements present

### Rate Limit Threshold (empirical)

- 4 requests in rapid succession: all returned 200
- 5th request: returned 202 (CAPTCHA)
- Once triggered, ALL subsequent rapid requests return 202
- Threshold appears to be ~4-5 requests within a few seconds from the same IP

### Current Implementation Gap

The code checks `response.status().is_success()` on line 180. HTTP 202 IS a success status (2xx), so the CAPTCHA page would be parsed as results. Since the CAPTCHA page has no `.result` elements, `parse_results()` would return an empty vector, and the output would be `(0 results)`.

This is not catastrophic but is misleading -- the user would see "0 results" instead of a clear message about rate limiting.

### Recommendation

Detect CAPTCHA responses explicitly:

```rust
let html = response.text().await?;

// Detect DDG CAPTCHA/bot challenge
if html.contains("anomaly-modal") || html.contains("bots use DuckDuckGo") {
    return Ok(ToolResult {
        content: "Search temporarily unavailable: DuckDuckGo rate limit reached. Try again in a few seconds.".into(),
        is_error: true,
        metadata: None,
    });
}
```

---

## 7. Cookies

### Response Headers

DDG's HTML endpoint returns **no Set-Cookie headers**. The response headers include:

```
server: nginx
content-type: text/html; charset=UTF-8
strict-transport-security: max-age=31536000
x-robots-tag: noindex
x-duckduckgo-locale: en_US
cache-control: max-age=1
```

No cookies are set or required. The current implementation correctly does not handle cookies. No cookie jar is needed in the reqwest client.

---

## 8. Response Size

| Query Type                                 | Response Size         | Result Count       |
| ------------------------------------------ | --------------------- | ------------------ |
| Technical query (rust async patterns)      | 30,332 bytes (~30KB)  | 10 organic         |
| Commercial query (buy cheap viagra online) | 43,651 bytes (~44KB)  | 2 ads + 10 organic |
| Special characters (c++ template)          | 30,290 bytes (~30KB)  | 10 organic         |
| Simple fact query (what is python)         | 44,443 bytes (~44KB)  | 12 organic         |
| CAPTCHA page                               | ~14,180 bytes (~14KB) | 0                  |

Typical responses are 30-45KB. This is well within reasonable limits for an HTTP client. No streaming or chunked handling is needed. The 15-second timeout in the current implementation is generous enough.

---

## 9. HTML Snippet Content

Snippets contain inline `<b>` tags for bold query term highlighting:

```html
<a class="result__snippet" href="..."
  >Learn how to choose between Tokio and <b>async</b>-std for <b>Rust</b>
  <b>async</b> programming...</a
>
```

The current implementation's `.text().collect::<String>()` correctly strips these tags to produce plain text. HTML entities like `&#x27;` (apostrophe), `&amp;` (ampersand) are also correctly decoded by the scraper crate.

Some snippets also contain a date span inside `result__extras`:

```html
<span>&nbsp; &nbsp; 2025-12-30T00:00:00.0000000</span>
```

This date is NOT inside the snippet element, so it does not affect snippet parsing. It could be extracted as additional metadata if desired.

---

## 10. Summary of Recommendations

### Must Fix (correctness issues)

| Issue                | Priority | Description                                                                                         |
| -------------------- | -------- | --------------------------------------------------------------------------------------------------- |
| Ad filtering         | High     | Filter `result--ad` class explicitly instead of relying on URL extraction failure                   |
| CAPTCHA detection    | High     | Detect HTTP 202 + `anomaly-modal` content and return clear error message                            |
| Update test snapshot | Medium   | Test uses `//duckduckgo.com/l/?uddg=` redirect pattern which no longer matches real organic results |

### Should Fix (robustness)

| Issue                                         | Priority | Description                                                |
| --------------------------------------------- | -------- | ---------------------------------------------------------- |
| Use `.result.web-result` selector             | Medium   | More specific than `.result` alone, naturally excludes ads |
| Check for empty results with specific message | Low      | Distinguish "no results for query" from "parsing failure"  |

### No Action Needed

| Aspect                      | Status                                                              |
| --------------------------- | ------------------------------------------------------------------- |
| `.result__a` selector       | Correct                                                             |
| `.result__snippet` selector | Correct                                                             |
| HTML entity handling        | Correct (scraper handles it)                                        |
| Special character encoding  | Correct (reqwest form encoding handles it)                          |
| Cookie handling             | Not needed (DDG sets no cookies)                                    |
| Response size               | Well within limits (30-45KB)                                        |
| Timeout (15s)               | Adequate                                                            |
| User-Agent string           | Works (Chrome UA accepted without CAPTCHA for normal request rates) |

### Implementation Changes (Prioritized)

1. **Replace `.result` with `.result.web-result`** in the selector to naturally exclude ads:

   ```rust
   let result_sel = Selector::parse(".result.web-result").expect("valid selector");
   ```

2. **Add CAPTCHA detection** after reading response body, before parsing:

   ```rust
   if html.contains("anomaly-modal") {
       return Ok(ToolResult {
           content: "Search rate limited by DuckDuckGo. Try again shortly.".into(),
           is_error: true,
           metadata: None,
       });
   }
   ```

3. **Simplify `extract_ddg_url()`** -- organic results now use direct URLs. The function can be simplified but keeping the redirect extraction is harmless (defensive coding in case DDG reverts).

4. **Update test snapshot** to reflect the real HTML structure with direct URLs, `rel="nofollow"`, and the actual class values.

5. **Add a test case for ad HTML** to verify ads are filtered correctly.

6. **Add a test case for CAPTCHA HTML** to verify the rate limit detection works.

---

## Appendix: Real HTML Response Excerpt (2026-02-08)

First organic result from `q=rust+async+patterns`:

```html
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title">
      <a
        rel="nofollow"
        class="result__a"
        href="https://doc.rust-lang.org/book/ch17-00-async-await.html"
      >
        Fundamentals of Asynchronous Programming: Async, Await ... - Learn Rust
      </a>
    </h2>
    <div class="result__extras">
      <div class="result__extras__url">
        <span class="result__icon">
          <a
            rel="nofollow"
            href="https://doc.rust-lang.org/book/ch17-00-async-await.html"
          >
            <img
              class="result__icon__img"
              width="16"
              height="16"
              alt=""
              src="//external-content.duckduckgo.com/ip3/doc.rust-lang.org.ico"
              name="i15"
            />
          </a>
        </span>
        <a
          class="result__url"
          href="https://doc.rust-lang.org/book/ch17-00-async-await.html"
        >
          doc.rust-lang.org/book/ch17-00-async-await.html
        </a>
      </div>
    </div>
    <a
      class="result__snippet"
      href="https://doc.rust-lang.org/book/ch17-00-async-await.html"
    >
      The <b>Rust</b> Programming Language Fundamentals of Asynchronous
      Programming: <b>Async</b>, Await, Futures, and Streams Many operations we
      ask the computer to do can take a while to finish.
    </a>
    <div class="clear"></div>
  </div>
</div>
```

First ad result from commercial query:

```html
<div class="result results_links results_links_deep result--ad ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title">
      <a
        rel="nofollow"
        class="result__a"
        href="https://duckduckgo.com/y.js?ad_domain=top10.com&ad_provider=bingv7aa&ad_type=txad&..."
      >
        Viagra Online - Best Viagra Deals Online
      </a>
    </h2>
    <div class="result__badge-wrap">
      <button class="badge--ad">Ad</button>
      <div class="badge--ad__tooltip-wrap">...</div>
    </div>
    ...
  </div>
</div>
```

CAPTCHA page (HTTP 202) key excerpt:

```html
<div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
<div class="anomaly-modal__description">
  Please complete the following challenge...
</div>
<div class="anomaly-modal__instructions">
  Select all squares containing a duck:
</div>
```
