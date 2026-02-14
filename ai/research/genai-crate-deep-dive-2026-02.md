# genai Crate Deep Dive (2026-02)

**Research Date**: 2026-02-14
**Crate**: [genai](https://crates.io/crates/genai) by Jeremy Chone
**GitHub**: https://github.com/jeremychone/rust-genai
**Docs**: https://docs.rs/genai/0.5.3/genai/
**License**: MIT OR Apache-2.0
**Stars**: 652 | **Forks**: 141 | **Downloads**: 119k total / 32k last 90 days

---

## Version & Maintenance

| Version        | Date       | Notes                                           |
| -------------- | ---------- | ----------------------------------------------- |
| 0.5.3 (stable) | 2026-01-31 | Current recommended                             |
| 0.6.0-alpha.2  | 2026-02-13 | Active development                              |
| 0.5.0          | 2026-01-10 | Big update: thinking, streaming engine refactor |
| 0.4.0          | ~2025      | Tool calls + thinking in streaming              |
| 0.1.11         | 2024-11    | First function calling pass                     |

Actively maintained. Author pushes frequently (last push 2026-02-13). 618 commits total. Breaks semver in 0.x patches (README warns to pin exact versions). Not yet 1.0.

---

## 1. Provider Support

### Natively Supported (14+)

| Provider       | Adapter   | Auth Env Var      |
| -------------- | --------- | ----------------- |
| OpenAI         | OpenAI    | OPENAI_API_KEY    |
| Anthropic      | Anthropic | ANTHROPIC_API_KEY |
| Google Gemini  | Gemini    | GEMINI_API_KEY    |
| xAI / Grok     | xAI       | XAI_API_KEY       |
| Ollama         | Ollama    | (none needed)     |
| Groq           | Groq      | GROQ_API_KEY      |
| DeepSeek       | DeepSeek  | DEEPSEEK_API_KEY  |
| Cohere         | Cohere    | COHERE_API_KEY    |
| Together       | Together  | TOGETHER_API_KEY  |
| Fireworks      | Fireworks | FIREWORKS_API_KEY |
| Nebius         | Nebius    | NEBIUS_API_KEY    |
| Mimo           | Mimo      | (unclear)         |
| Zai (Zhipu AI) | Zai       | (unclear)         |
| BigModel       | BigModel  | (unclear)         |

### Custom Providers via ServiceTargetResolver

Any OpenAI-compatible endpoint (including OpenRouter) can be used via `ServiceTargetResolver`:

```rust
let target_resolver = ServiceTargetResolver::from_resolver_fn(
    |service_target: ServiceTarget| -> Result<ServiceTarget> {
        let endpoint = Endpoint::from_static("https://api.together.xyz/v1/");
        let auth = AuthData::from_env("TOGETHER_API_KEY");
        let model = ModelIden::new(AdapterKind::OpenAI, service_target.model.model_name);
        Ok(ServiceTarget { endpoint, auth, model })
    }
);
let client = Client::builder()
    .with_service_target_resolver(target_resolver)
    .build();
```

### Missing Providers

- **AWS Bedrock**: Open issue #88, planned but not implemented
- No native OpenRouter adapter (must use ServiceTargetResolver with OpenAI adapter)

---

## 2. Authentication

### AuthData Enum

```rust
enum AuthData {
    FromEnv(String),       // Read key from env var
    Key(String),           // Direct key value
    RequestOverride,       // Custom auth via headers
    MultiKeys(HashMap),    // Multiple credential parts (adapter-specific)
}
```

Construction: `AuthData::from_env("KEY_NAME")`, `AuthData::from_single("sk-...")`, `AuthData::from_multi(map)`

### AuthResolver

Custom auth logic per model:

```rust
Client::builder()
    .with_auth_resolver(AuthResolver::from_resolver_fn(
        |model_iden: ModelIden| -> Result<AuthData> {
            // Return auth based on model/provider
            Ok(AuthData::from_env("MY_KEY"))
        }
    ))
    .build()
```

### Limitations

- No built-in OAuth flow (only API keys and env vars)
- No token refresh mechanism
- `RequestOverride` variant exists but has reported bugs with Gemini (#70)

---

## 3. Streaming

**Supported**: Yes, first-class. Both `exec_chat()` (non-streaming) and `exec_chat_stream()` (streaming).

### ChatStreamEvent Enum

```rust
enum ChatStreamEvent {
    Start,                              // Stream begins
    Chunk(StreamChunk),                 // Text content delta
    ReasoningChunk(StreamChunk),        // Reasoning/thinking delta
    ThoughtSignatureChunk(StreamChunk), // Thought signature delta
    ToolCallChunk(ToolChunk),           // Tool call delta
    End(StreamEnd),                     // Stream ends, has usage + captured content
}
```

### ChatStream

Implements `futures::Stream<Item = Result<ChatStreamEvent>>`. Standard async stream consumption:

```rust
let chat_res = client.exec_chat_stream(model, chat_req, None).await?;
let mut stream = chat_res.stream;
while let Some(event) = stream.next().await {
    match event? {
        ChatStreamEvent::Chunk(chunk) => print!("{}", chunk.content),
        ChatStreamEvent::End(end) => { /* usage, captured content */ },
        _ => {}
    }
}
```

### Capture Options (via ChatOptions)

- `capture_usage`: Aggregate token counts in StreamEnd
- `capture_content`: Concatenate text chunks into final string
- `capture_reasoning_content`: Concatenate reasoning chunks
- `capture_tool_calls`: Collect tool call events
- `capture_raw_body`: Raw HTTP body for debugging

### Internal Architecture

v0.5.0 refactored to unified `EventSourceStream` + `WebStream` for all providers. Prior versions had per-adapter streaming bugs.

### Known Issues

- #104: Chinese character truncation (UTF-8 encoding issue in streaming)
- #106: Ollama streaming empty chunks for reasoning models (qwen3, deepseek-r1)
- #112: Intermittent "tool_calls must be followed by tool messages" with OpenAI

---

## 4. Tool / Function Calling

**Supported**: Yes, since v0.1.11 (OpenAI/Anthropic). Expanded in v0.4.0+ to streaming.

### Tool Definition

```rust
let tool = Tool::new("get_weather")
    .with_description("Get weather for a location")
    .with_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "location": {"type": "string"}
        },
        "required": ["location"]
    }));
```

### Sending Tools

```rust
let chat_req = ChatRequest::from_user("What's the weather?")
    .with_tools([tool]);
```

### Receiving Tool Calls

```rust
struct ToolCall {
    call_id: String,            // Correlation ID
    fn_name: String,            // Function name
    fn_arguments: Value,        // JSON arguments
    thought_signatures: Option<Vec<String>>,
}
```

### Multi-turn Tool Loop

```rust
// After receiving tool call in StreamEnd:
let chat_req = chat_req
    .append_tool_use_from_stream_end(&end, tool_response);
// Then exec again
```

### Adapters with Tool Support

OpenAI, Anthropic, Ollama, Gemini confirmed. Others likely via OpenAI-compat.

### Known Issues & Limitations

- **#60**: ToolCalls and Text in one response -- cannot receive both text and tool calls in a single response. This is a critical limitation for agent UX where models comment on their tool usage. Labeled `planned-feature, API-CHANGE`.
- **#134**: Anthropic custom tool hijacking and stale streamer state
- **#87**: Potential bug with built-in tools in Gemini
- **#112**: Intermittent OpenAI tool call ordering errors
- **#118**: OpenAI Responses API wrong content type for tool responses (marked FIXED)

---

## 5. Thinking / Extended Thinking

**Supported**: Yes, for both Anthropic and Gemini.

### Anthropic

- `ReasoningEffort` control (since v0.5.0): `ReasoningEffort::None`, and presumably Low/Medium/High
- Reasoning content extracted into separate `ReasoningChunk` stream events
- Thought signatures captured separately
- `ChatOptions::normalize_reasoning_content`: Extract reasoning blocks into dedicated field

### Gemini

- Gemini Thinking support with full thought signatures (v0.5.0)
- Thinking levels via `ReasoningEffort`

### Limitations

- No `budget_tokens` control visible in the API (ion uses this for Anthropic)
- ReasoningEffort is a hint, not a precise token budget

---

## 6. HTTP Client Customization

### WebConfig

```rust
WebConfig::default()
    .with_timeout(Duration::from_secs(60))
    .with_connect_timeout(Duration::from_secs(10))
    .with_proxy_url("http://proxy:8080")
    .with_https_proxy_url("https://proxy:8443")
    .with_all_proxy_url("http://proxy:8080")
    .with_default_headers(header_map)
```

Apply via: `Client::builder().with_web_config(web_config).build()`

### Custom Headers

Two levels:

1. **WebConfig**: Default headers for all requests
2. **ChatOptions**: `extra_headers` per-request override

### Base URL / Endpoint

Via `ServiceTargetResolver`: full control over endpoint URL per model/request.

### Limitations

- No direct access to underlying `reqwest::Client` after construction
- No custom TLS configuration exposed
- `apply_to_builder()` method exists but you cannot inject a pre-built reqwest client

---

## 7. API Surface Summary

### Key Types

| Type                 | Purpose                                                |
| -------------------- | ------------------------------------------------------ |
| `Client`             | Primary interface, built via `ClientBuilder`           |
| `ChatRequest`        | Messages + tools + system prompt                       |
| `ChatResponse`       | Non-streaming result                                   |
| `ChatStream`         | `futures::Stream` of events                            |
| `ChatStreamResponse` | Contains the stream + metadata                         |
| `ChatOptions`        | Temperature, max_tokens, top_p, headers, capture flags |
| `Tool`               | Function definition (name, description, schema)        |
| `ToolCall`           | Model's invocation request                             |
| `ToolResponse`       | Your execution result                                  |
| `StreamEnd`          | Terminal event with usage + captured content           |
| `ServiceTarget`      | Endpoint + auth + model identity                       |
| `AuthData`           | API key or env var reference                           |

### Client Methods

```rust
client.exec_chat(model, request, options) -> Result<ChatResponse>
client.exec_chat_stream(model, request, options) -> Result<ChatStreamResponse>
```

---

## 8. Capability Matrix: genai vs ion Custom Provider

| Capability                      | genai 0.5.3                          | ion Custom (current)                                                  |
| ------------------------------- | ------------------------------------ | --------------------------------------------------------------------- |
| **Providers**                   | 14+ native + custom resolver         | 8 (Anthropic, OpenAI, Google, Gemini, OpenRouter, Groq, Kimi, Ollama) |
| **Streaming**                   | Yes (futures::Stream)                | Yes (mpsc channels)                                                   |
| **Tool calling**                | Yes (all major providers)            | Yes (all providers)                                                   |
| **Tool + text in one response** | No (#60 open)                        | Yes (ContentBlock enum)                                               |
| **Thinking/reasoning**          | Yes (Anthropic + Gemini)             | Yes (Anthropic + Google)                                              |
| **Budget tokens**               | No (ReasoningEffort only)            | Yes (ThinkingConfig.budget_tokens)                                    |
| **Custom base URL**             | Yes (ServiceTargetResolver)          | Yes (with_base_url)                                                   |
| **Custom headers**              | Yes (WebConfig + ChatOptions)        | Manual per-backend                                                    |
| **OAuth / token refresh**       | No                                   | Yes (ChatGPT, Gemini OAuth)                                           |
| **Provider quirks**             | Per-adapter normalization            | Explicit quirks.rs                                                    |
| **Cache control**               | Not visible                          | Yes (Anthropic cache_control)                                         |
| **Image/vision**                | Yes (OpenAI, Gemini, Anthropic)      | Yes (ContentBlock::Image)                                             |
| **Embeddings**                  | Yes (basic)                          | No (not needed)                                                       |
| **Model registry/discovery**    | Auto from model name prefix          | JSON registry + API fetch                                             |
| **Error detail**                | Captures request/response on failure | Custom error types                                                    |
| **Proxy support**               | Yes (WebConfig)                      | No                                                                    |
| **PDF/binary**                  | Yes (v0.5.0)                         | No                                                                    |
| **Token usage**                 | Yes (normalized)                     | Yes (Usage struct)                                                    |
| **Response format**             | JSON mode support                    | Not exposed                                                           |
| **Responses API (OpenAI)**      | Partial (#118 bugs)                  | Yes (ChatGPT subscription)                                            |
| **Code size**                   | ~8.5k SLoC (lib)                     | ~9k SLoC (provider module)                                            |
| **Maturity**                    | Pre-1.0, breaks in patches           | Production, stable internal API                                       |
| **Async runtime**               | tokio                                | tokio                                                                 |

---

## 9. Assessment

### What genai Does Well

1. **Breadth of providers**: 14+ adapters, more than ion needs, with a clean resolver pattern for custom endpoints
2. **Streaming architecture**: Unified EventSourceStream is clean, futures::Stream is ergonomic
3. **Thinking/reasoning**: First-class support for both Anthropic and Gemini
4. **HTTP customization**: Proxy support, custom headers, timeout configuration
5. **Active maintenance**: Regular releases, responsive to issues

### Critical Gaps for ion

1. **No text + tool calls in one response (#60)**: This is a showstopper. ion's agent loop relies on models emitting text alongside tool calls (e.g., "Let me read that file" + tool_call). genai forces either/or. Labeled as planned but would be an API-CHANGE.

2. **No OAuth support**: ion has ChatGPT subscription (OAuth + Responses API) and Gemini OAuth. genai only does API keys.

3. **No budget_tokens for thinking**: ion passes explicit token budgets to Anthropic. genai only has ReasoningEffort (a coarse hint).

4. **No cache_control**: ion uses Anthropic's prompt caching (cache_control on system/tool messages). Not visible in genai's API.

5. **No Responses API**: ion supports ChatGPT subscription via OpenAI Responses API. genai has #118 bugs with it.

6. **Pre-1.0 instability**: README warns to pin exact versions. Patches break APIs. Would need to vendor or track closely.

7. **Channel vs Stream mismatch**: ion uses `mpsc::Sender<StreamEvent>` for streaming (push model). genai uses `futures::Stream` (pull model). Migration would require adapter layer or agent loop rewrite.

### What Migration Would Cost

- Rewrite agent loop to consume `futures::Stream` instead of `mpsc::Receiver`
- Implement OAuth wrapper outside genai (or keep ion's auth module)
- Work around text+tool limitation (parse stream events manually, accumulate both)
- Lose cache_control support or add it via provider-specific escape hatches
- Lose budget_tokens precision (ReasoningEffort is coarser)
- Adapt ContentBlock <-> genai ChatMessage/ToolCall type mapping
- Maintain ServiceTargetResolver configs for OpenRouter, ChatGPT, Gemini

### Verdict

**Do not adopt genai for ion.** The critical gaps (text+tool in one response, OAuth, cache_control, budget_tokens) would require extensive workarounds that negate the benefit of using a library. ion's custom provider layer is ~9k SLoC but handles exactly what ion needs. genai is a good library for simpler chat applications, but ion's agent requirements exceed its current capabilities.

genai is worth watching for when it hits 1.0 and resolves #60. If those gaps close, reconsidering would be reasonable.

---

## Sources

- https://crates.io/crates/genai
- https://github.com/jeremychone/rust-genai
- https://docs.rs/genai/0.5.3/genai/
- https://github.com/jeremychone/rust-genai/issues/60 (text + tool calls)
- https://github.com/jeremychone/rust-genai/issues/88 (Bedrock)
- https://github.com/jeremychone/rust-genai/issues/70 (Gemini auth override bug)
