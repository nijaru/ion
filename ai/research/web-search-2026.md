# Web Search Tool Research (February 2026)

**Research Date**: 2026-02-07
**Purpose**: Evaluate approaches for adding a web search tool to ion without requiring an API key
**Task**: tk-75jw

---

## Executive Summary

| Approach                      | API Key     | Free Tier    | Result Quality | Reliability | Complexity   |
| ----------------------------- | ----------- | ------------ | -------------- | ----------- | ------------ |
| **DuckDuckGo HTML scraping**  | No          | Unlimited\*  | Good           | Medium      | Low          |
| **Brave Search API**          | Yes (free)  | 2,000/mo     | Excellent      | High        | Low          |
| **SearXNG (public instance)** | No          | Varies       | Good           | Low         | Low          |
| **SearXNG (self-hosted)**     | No          | Unlimited    | Good           | High        | High (infra) |
| **Google Custom Search**      | Yes         | 100/day      | Excellent      | High        | Low          |
| **Provider-native search**    | Via LLM key | Via LLM plan | Excellent      | High        | Medium       |

**Recommendation**: DuckDuckGo HTML scraping as primary (zero-config), with optional Brave Search API key for better quality. See full recommendation in Section 5.

---

## 1. How Other Agents Do Web Search

### Claude Code

Two built-in tools with different purposes:

| Tool          | Mechanism                         | Model Used                 | Purpose                     |
| ------------- | --------------------------------- | -------------------------- | --------------------------- |
| **WebSearch** | Anthropic server-side search API  | Claude (encrypted results) | Find sources for a query    |
| **WebFetch**  | Local Axios fetch + Haiku summary | Claude Haiku               | Extract info from known URL |

**WebSearch** uses Anthropic's proprietary `web_search_tool` server-side. Results come back as `web_search_tool_result` with encrypted content (not readable by users). This is tightly coupled to the Anthropic API.

**WebFetch** fetches pages locally from the user's machine using Axios, converts HTML to markdown, then sends to Claude Haiku for summarization. Has a domain deny-list for SSRF protection and ~80 pre-approved documentation domains with simplified prompts.

Key insight: Claude Code's WebSearch is not replicable -- it uses Anthropic's internal search infrastructure. WebFetch (URL fetching + LLM summarization) is the replicable pattern.

### Gemini CLI

Uses **Google Search Grounding** -- a native Gemini API feature. The model decides when to search, sends the query to Google Search via the API, and receives grounded results with citations. This is a provider-specific feature, not a standalone tool.

- Enabled via `tools: [{ google_search: {} }]` in the API config
- Reduces hallucinations by ~40% according to Google
- Free with Gemini free tier (billing starts Jan 2026)
- Not replicable outside the Gemini ecosystem

### OpenCode

- **WebFetch** tool exists (URL fetching, similar to Claude Code's)
- **WebSearch** was requested (issue #309) but not fully implemented as built-in
- Currently uses Exa API behind `OPENCODE_ENABLE_EXA` flag (requires key)
- Community discussion suggests DuckDuckGo via `ddgs` Python library as MCP wrapper
- General sentiment: web search should be provider-native or MCP-delegated

### Codex CLI (OpenAI)

- No built-in web search tool in the CLI
- The cloud Codex Web product has internet access
- Relies on OpenAI's model capabilities (web browsing in ChatGPT)

### Aider

- No built-in web search
- Users integrate via external tools (Vectorshift, custom RAG pipelines)
- Focused purely on code editing, not information retrieval

### Goose (Block)

- No built-in web search
- Uses MCP extensively (3,000+ tool connections)
- Users add search via MCP servers (Brave Search, Tavily, etc.)
- Extension manager can dynamically discover and enable MCP servers

### Summary

| Agent       | Built-in Search            | Mechanism                           | Replicable?   |
| ----------- | -------------------------- | ----------------------------------- | ------------- |
| Claude Code | Yes (WebSearch + WebFetch) | Proprietary API + local fetch       | WebFetch only |
| Gemini CLI  | Yes (Search Grounding)     | Google Search API (provider-native) | No            |
| OpenCode    | Partial (WebFetch + Exa)   | URL fetch + paid API                | WebFetch only |
| Codex CLI   | No                         | N/A                                 | N/A           |
| Aider       | No                         | N/A                                 | N/A           |
| Goose       | No (via MCP)               | MCP servers                         | Via MCP       |

**Pattern**: Most agents either use provider-native search (tied to their LLM provider) or delegate to MCP. No agent has a robust, provider-agnostic, zero-config web search.

---

## 2. DuckDuckGo Options

### 2a. DuckDuckGo HTML Scraping

The most viable zero-config approach. Multiple implementations exist in the wild.

**Endpoints**:

| Endpoint                    | JS Required | Pagination        | Best For                  |
| --------------------------- | ----------- | ----------------- | ------------------------- |
| `html.duckduckgo.com/html/` | No          | Form-based (POST) | Primary scraping target   |
| `lite.duckduckgo.com/lite/` | No          | Simple links      | Lightweight alternative   |
| `duckduckgo.com/`           | Yes         | Infinite scroll   | Not suitable for scraping |

**Request format** (html.duckduckgo.com):

```
POST https://html.duckduckgo.com/html/
Content-Type: application/x-www-form-urlencoded

q=search+query&kl=us-en&df=m
```

Parameters:

- `q`: Search query (required)
- `kl`: Region code (e.g., `us-en`, `uk-en`)
- `df`: Date filter (`d`=day, `w`=week, `m`=month, `y`=year)

**HTML selectors for results**:

- Container: `#links .result`
- Title + URL: `.result__a` (href attribute, URLs are protocol-relative `//...`)
- Display URL: `.result__url`
- Snippet: `.result__snippet`

**Pagination**: Form-based -- extract hidden fields from `.nav-link form` and POST again. Approximately 30 results per page.

**lite.duckduckgo.com** alternative:

- Even simpler HTML, ~10KB compressed per page
- Table-based layout with basic `<a>` links and text
- Lower scraping complexity
- Same result quality

**Rate limiting / anti-bot measures**:

- No explicit rate limit header
- Aggressive scraping triggers: HTTP 403, CAPTCHAs, empty results
- Mitigation: random delays (1-3s), rotating User-Agent, reasonable request frequency
- For a coding agent doing occasional searches (a few per session), risk is very low

**Existing Rust crates**:

| Crate                        | Downloads | Approach                                  | Quality            |
| ---------------------------- | --------- | ----------------------------------------- | ------------------ |
| `duckduckgo` (v0.2.0)        | 5,112     | HTML scraping, Auto/HTML/API backends     | Decent, active     |
| `duckduckgo_rs` (v0.0.1)     | Low       | HTML scraping                             | Minimal, v0.0.1    |
| `duckduckgo_search` (v0.1.3) | 2,864     | HTML scraping                             | Basic              |
| `websearch` (v0.1.1)         | 3,499     | Multi-provider (DDG, Google, Brave, etc.) | Most comprehensive |

The `duckduckgo` crate by kevin-rs:

- Uses `https://html.duckduckgo.com/html/` via POST
- Random User-Agent via `fake_useragent`
- XPath-based parsing: `//div[contains(@class, 'body')]` for items
- Filters out DuckDuckGo redirect URLs (`https://duckduckgo.com/y.js?`)

The `websearch` crate:

- Multi-provider: DuckDuckGo (no key), Google (key), Brave (key), Tavily (key), SearXNG (no key), ArXiv (no key)
- Standardized result format: title, url, snippet
- Most feature-complete but heavier dependency

**Recommendation**: Implement DDG HTML scraping directly (small scope, no external crate dependency needed). The HTML structure is simple and stable. Use `reqwest` + `scraper` crate for parsing.

### 2b. DuckDuckGo Instant Answer API

**Endpoint**: `https://api.duckduckgo.com/?q=query&format=json`

This is **not** a general web search API. It returns structured "instant answer" data:

- Wikipedia summaries
- Calculator results
- Weather, sports, recipes
- Definitions, factual lookups

**Limitations**:

- Does NOT return web search results (links, snippets)
- Returns zero results for most queries a coding agent would make
- Only useful for factual lookups ("what is X")
- No authentication required

**Verdict**: Not suitable for a web search tool. It solves a different problem.

### 2c. DuckDuckGo TOS Considerations

DuckDuckGo's Terms of Service do not explicitly prohibit scraping, but they don't endorse it either. The HTML endpoints have been stable for years and are widely used by privacy tools. Key points:

- The `lite` and `html` endpoints exist specifically for low-bandwidth/accessibility use
- The Python `duckduckgo-search` library (renamed to `ddgs`) has been actively maintained since 2021 with millions of downloads
- Risk is low for reasonable, non-commercial use at agent-level volumes (a few searches per session)

---

## 3. Alternative Free Search Approaches

### Brave Search API

**Free tier**: 2,000 queries/month, 1 query/second
**Requires**: API key + credit card (anti-fraud, not charged)
**Endpoint**: `https://api.search.brave.com/res/v1/web/search?q=query`
**Response**: Clean JSON with title, url, description, age
**Quality**: Excellent -- independent index covering 35B+ pages

Pros:

- Clean, well-documented REST API
- High result quality from independent index
- Structured JSON response (no parsing needed)
- Additional endpoints: news, images, videos, suggestions
- AI Grounding API for enhanced results

Cons:

- Requires API key registration
- Credit card required even for free tier
- 2,000/month limit (adequate for personal use)
- Dependency on external service availability

**Verdict**: Best option if users are willing to register for an API key. Natural "upgrade path" from DDG scraping.

### SearXNG

**Self-hosted**: Unlimited, full control
**Public instances**: Variable availability, many disable JSON format
**Endpoint**: `GET /search?q=query&format=json`
**Response**: JSON with title, url, content, engine, score

Pros:

- Fully open source, privacy-focused
- Aggregates from 249+ search engines
- JSON API when enabled
- No rate limits on self-hosted instances

Cons:

- Public instances are unreliable (many disable API/JSON format)
- Self-hosting requires infrastructure (Docker, server)
- Result quality depends on which engines are configured
- Not practical as a zero-config default

**Verdict**: Good for power users who self-host. Not suitable as default -- too much setup.

### Google Custom Search API

**Free tier**: 100 queries/day
**Requires**: API key + Search Engine ID
**Important**: Closed to new customers as of 2026. Existing customers must migrate by Jan 2027.
**Alternative**: Vertex AI Search

**Verdict**: Deprecated, not recommended for new projects.

### Provider-Native Search

Some LLM providers offer built-in search:

| Provider      | Feature          | How                                   |
| ------------- | ---------------- | ------------------------------------- |
| Google/Gemini | Search Grounding | `tools: [{ google_search: {} }]`      |
| Anthropic     | Web Search       | `web_search_tool` (beta, server-side) |
| Groq          | Built-in Exa     | Available for specific models         |
| OpenAI        | Web browsing     | ChatGPT integration only              |

These require no additional API keys (bundled with LLM access) but are provider-specific. ion already supports multiple providers, so this could work as an enhancement per-provider.

### Other Paid APIs (for reference)

| API        | Free Tier      | Cost/1K | Notes                         |
| ---------- | -------------- | ------- | ----------------------------- |
| Serper     | 2,500 one-time | $0.30   | Scrapes Google, fast          |
| Tavily     | 1,000/mo       | ~$1.00  | AI-optimized, LLM-friendly    |
| Search1API | 100 credits    | Varies  | Multi-engine, keyless demo    |
| Exa        | Limited        | ~$3.00  | Semantic search, code-focused |
| DataForSEO | $50 deposit    | $0.60   | Google scraping               |

---

## 4. Technical Considerations

### Result Structure

Standard result format used across implementations:

```rust
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}
```

Additional optional fields: `age` (page date), `source` (search engine), `favicon_url`.

### Number of Results

- Claude Code WebSearch: Returns ~5-10 results
- Most search APIs default to 10 results per query
- For a coding agent: 5-8 results is optimal (enough context without overwhelming)
- DuckDuckGo HTML returns ~30 per page; take first 8-10

### Content Extraction (WebFetch companion)

For fetching and extracting content from URLs found via search:

**Rust crates**:

- `html2text` (v0.16.5, 1.9M downloads) -- HTML to plain text, uses html5ever
- `readability-rust` (v0.1.0) -- Mozilla Readability.js port, extracts article content
- `scraper` -- CSS selector-based HTML parsing
- `reqwest` -- HTTP client (already used by ion)

**Approach** (following Claude Code's WebFetch pattern):

1. Fetch URL with reqwest
2. Extract main content with readability or html2text
3. Truncate to reasonable size (e.g., 8K chars)
4. Return as tool result for the LLM to process

### Rate Limiting / Caching

- Cache search results for same query within session (avoid duplicate searches)
- DuckDuckGo: add 1-2s delay between searches (only matters for burst usage)
- For API-based providers: respect rate limits, use exponential backoff
- Simple in-memory LRU cache with 15-minute TTL (matches Claude Code)

### User-Agent Handling

- Use a realistic browser User-Agent string
- Rotate from a small pool (3-5 agents) to avoid fingerprinting
- Example: `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36`

### SSRF Protection

Critical for any URL-fetching tool. The agent must not be tricked into fetching internal/private resources.

**Must block**:

- Private IPv4: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`
- Loopback: `127.0.0.0/8`, `::1`
- Link-local: `169.254.0.0/16`, `fe80::/10`
- Cloud metadata: `169.254.169.254` (AWS/GCP/Azure)
- Non-HTTP schemes: `file://`, `ftp://`, `gopher://`

**Implementation**:

1. Parse URL, validate scheme is `http` or `https`
2. Resolve hostname to IP address
3. Check resolved IP against blocklist before connecting
4. Follow redirects only to same or public hosts (re-validate each redirect)
5. Set reasonable timeout (10s) and max response size (1MB)

---

## 5. Recommendation

### Recommended Architecture: Two Tools

**Tool 1: `web_search`** -- Find information via search engine
**Tool 2: `web_fetch`** -- Extract content from a known URL

This mirrors Claude Code's proven pattern and gives the LLM maximum flexibility.

### Primary Search Backend: DuckDuckGo HTML Scraping

**Why DuckDuckGo HTML scraping as default**:

- Zero configuration required (no API key, no registration)
- Works immediately out of the box
- Adequate result quality for coding-related queries
- Stable endpoints used by many tools for years
- Privacy-respecting (aligns with a local-first agent)
- Low implementation complexity (POST + HTML parse)

**Implementation plan**:

1. POST to `https://html.duckduckgo.com/html/` with query
2. Parse response with `scraper` crate (CSS selectors)
3. Extract title, URL, snippet from `.result__a`, `.result__url`, `.result__snippet`
4. Return first 8 results as structured JSON
5. Realistic User-Agent header, no cookies needed

### Optional API Key Upgrade: Brave Search

Allow users to configure `BRAVE_API_KEY` in config for higher-quality results:

- Falls back to DuckDuckGo if no key configured
- Clean REST API, easy to implement
- 2,000 free queries/month is generous for personal use

### Config Structure

```toml
[tools.web_search]
provider = "auto"  # "duckduckgo" | "brave" | "searxng"
brave_api_key = ""  # optional, from env BRAVE_API_KEY
searxng_url = ""    # optional, self-hosted instance URL
max_results = 8
```

With `"auto"`: use Brave if key is set, otherwise DuckDuckGo.

### Implementation Scope

**Minimal viable implementation** (web_search only):

| Component       | Crate                | Purpose                    |
| --------------- | -------------------- | -------------------------- |
| HTTP client     | `reqwest` (existing) | POST to DDG, GET for Brave |
| HTML parsing    | `scraper`            | Parse DDG HTML results     |
| URL validation  | `url` (existing)     | Parse and validate URLs    |
| SSRF protection | Custom (small)       | IP blocklist check         |

**Later additions**:

- `web_fetch` tool (URL content extraction)
- Brave Search backend
- SearXNG backend
- Provider-native search (Gemini grounding, Anthropic web search)
- Result caching

### Why NOT use existing crates

The `duckduckgo` and `websearch` crates are low-quality (v0.0.1-v0.2.0, minimal downloads, no recent activity). The scraping logic is simple enough (~100 lines) that a direct implementation with `reqwest` + `scraper` is preferable to taking a dependency on an unmaintained crate.

### Estimated Complexity

| Component            | Lines    | Effort           |
| -------------------- | -------- | ---------------- |
| DDG scraping         | ~100     | Small            |
| Result types         | ~30      | Trivial          |
| Tool definition      | ~50      | Small            |
| SSRF protection      | ~60      | Small            |
| Brave API (optional) | ~80      | Small            |
| **Total**            | **~320** | **1-2 sessions** |

---

## References

**Agent implementations**:

- Claude Code WebSearch/WebFetch internals: https://mikhail.io/2025/10/claude-code-web-tools/
- Claude Code reverse engineering: https://quercle.dev/blog/claude-code-web-tools
- OpenCode web search discussion: https://github.com/anomalyco/opencode/issues/309
- Gemini Search Grounding: https://ai.google.dev/gemini-api/docs/google-search

**DuckDuckGo**:

- HTML scraping guide: https://roundproxies.com/blog/scrape-duckduckgo/
- lite.duckduckgo.com architecture: https://guessless.dev/blog/service-level-architecture/lite.duckduckgo.com
- Instant Answer API: https://duckduckgo.com/api
- Python ddgs library: https://pypi.org/project/ddgs/

**Rust crates**:

- `duckduckgo` crate: https://crates.io/crates/duckduckgo
- `websearch` crate: https://crates.io/crates/websearch
- `html2text`: https://crates.io/crates/html2text
- `readability-rust`: https://docs.rs/readability-rust

**Search APIs**:

- Brave Search API: https://brave.com/search/api/
- SearXNG API docs: https://docs.searxng.org/dev/search_api.html
- Google Custom Search (deprecated): https://developers.google.com/custom-search/v1/overview
- Search API comparison: https://medium.com/@fardeenxyz/8-web-search-apis-you-need-to-know-about

**Security**:

- OWASP SSRF Prevention: https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html
