# Prompt Caching Across LLM Providers

**Research Date:** 2026-01-13
**Focus:** Multi-turn conversations, parallel API calls, subagent patterns

## Executive Summary

| Provider   | Cache Type             | Multi-turn Benefit  | Parallel Call Benefit                | Subagent Benefit |
| ---------- | ---------------------- | ------------------- | ------------------------------------ | ---------------- |
| Anthropic  | Explicit (breakpoints) | High                | Low (requires sequential first call) | Medium           |
| OpenAI     | Automatic (prefix)     | High                | High (automatic routing)             | High             |
| Gemini     | Implicit + Explicit    | High                | Medium (implicit auto)               | High (explicit)  |
| DeepSeek   | Automatic (prefix)     | Very High           | High                                 | Very High        |
| OpenRouter | Pass-through           | Depends on provider | Depends on provider                  | Varies           |

---

## 1. Anthropic Prompt Caching

### Mechanism

- **Type:** Explicit with `cache_control` breakpoints
- **Cache Key:** Cumulative hash of all blocks up to and including the cached block
- **Matching:** Exact prefix match required (100% identical)
- **TTL:** 5 minutes default, 1 hour available at 2x cost
- **Min Tokens:** 1024 (Sonnet/Opus 4.x), 4096 (Opus 4.5, Haiku 4.5), 2048 (Haiku 3.x)

### Pricing (multipliers of base input)

| Operation   | 5-min TTL | 1-hour TTL |
| ----------- | --------- | ---------- |
| Cache Write | 1.25x     | 2.0x       |
| Cache Read  | 0.1x      | 0.1x       |

### Multi-turn Conversation Behavior

- **Yes, benefits significantly** - Each turn preserves the cached prefix
- Place `cache_control` on the system message and/or static context
- Conversation history grows but prefix remains cached
- 10-turn conversation with long system prompt: ~75% latency reduction, ~53% cost reduction

### Parallel API Calls (RLM partition+map)

- **Limited benefit** - Cache entry only becomes available after first response begins
- **Critical constraint:** "For concurrent requests, note that a cache entry only becomes available after the first response begins. If you need cache hits for parallel requests, wait for the first response before sending subsequent requests."
- **Workaround:** Send first request, wait for streaming to start, then send parallel requests

### Subagent Patterns

- **Organization isolation:** Caches are isolated between organizations (good for security)
- **Same org subagents:** Can share cache if using identical prefixes
- **Recommendation:** Have subagents share a common cached system prompt prefix
- **Thinking blocks:** Automatically cached when passed back in tool use flows

### Implementation Details

```json
{
  "system": [{
    "type": "text",
    "text": "Large system prompt here...",
    "cache_control": {"type": "ephemeral"}
  }],
  "messages": [...]
}
```

### Best Practices

1. Static content at beginning (system prompt, tools, examples)
2. Dynamic content at end (user messages)
3. Use breakpoints strategically at natural boundaries
4. Monitor `cache_read_input_tokens` and `cache_creation_input_tokens`

---

## 2. OpenRouter Caching

### Mechanism

- **Type:** Pass-through to underlying providers
- **Routing:** Best-effort to route to same provider for warm cache hits
- **Fallback:** Routes to next-best provider if cached provider unavailable

### Provider-Specific Behavior via OpenRouter

| Provider          | Cache Writes    | Cache Reads | Configuration                 |
| ----------------- | --------------- | ----------- | ----------------------------- |
| Anthropic         | 1.25x           | 0.1x        | Requires `cache_control`      |
| OpenAI            | Free            | 0.25-0.5x   | Automatic (1024+ tokens)      |
| DeepSeek          | 1.0x            | 0.1x        | Automatic                     |
| Gemini (implicit) | Free            | 0.25x       | Automatic (1028-4096+ tokens) |
| Gemini (explicit) | Input + storage | 0.25x       | Requires `cache_control`      |
| Grok              | Free            | 0.25x       | Automatic                     |

### Multi-turn/Parallel/Subagent

- Behavior matches underlying provider
- OpenRouter adds routing optimization for cache locality
- `cache_discount` field in response shows savings

### OpenRouter-Specific Features

- Activity page shows cache usage per request
- `/api/v1/generation` API for cache metrics
- `usage: {include: true}` returns cache tokens in response

---

## 3. Google Gemini Context Caching

### Two Caching Modes

#### Implicit Caching (Automatic)

- **Type:** Automatic, no configuration needed
- **Availability:** Gemini 2.5 Flash, 2.5 Pro, 3 Flash Preview, 3 Pro Preview
- **Min Tokens:** 1024 (Flash), 4096 (Pro)
- **TTL:** ~3-5 minutes average (varies)
- **Cost:** No write cost, reads at 0.25x input price

#### Explicit Caching (Manual)

- **Type:** Named cache objects with explicit TTL
- **Default TTL:** 1 hour (configurable)
- **Cost:** Charged for storage duration + 0.25x for reads
- **API:** `client.caches.create()`, `client.caches.get()`, `client.caches.delete()`

### Multi-turn Conversation Behavior

- **Implicit:** Benefits automatically if prefix is consistent
- **Explicit:** Create cache once, reference in all subsequent turns
- Explicit caching ideal for: long documents, video analysis, system instructions

### Parallel API Calls

- **Implicit:** High benefit - automatic prefix matching works across parallel calls
- **Explicit:** Very high benefit - named cache guaranteed available to all calls

### Subagent/Agent Patterns

- **Explicit caching is ideal for agents** - Create cache for:
  - System instructions shared across agent calls
  - Large documents being analyzed
  - Video/image context
- All subagents can reference same named cache
- No organization isolation mentioned (verify for production)

### Implementation (Explicit)

```python
# Create cache
cache = client.caches.create(
    model="gemini-2.5-pro",
    display_name="my_cache",
    contents=[...],
    ttl=timedelta(hours=1)
)

# Use cache
response = client.models.generate_content(
    model="gemini-2.5-pro",
    cached_content=cache.name,
    contents=[user_message]
)
```

### Best Practices

1. Use implicit for standard chat/conversational workloads
2. Use explicit when you need guaranteed cost savings
3. Put variable content at end of prompts for implicit cache hits
4. For agents: create explicit cache at session start, reference throughout

---

## 4. DeepSeek Caching

### Mechanism

- **Type:** Automatic disk-based KV cache
- **Cache Key:** Exact prefix match (from token 0)
- **Storage:** Distributed disk array (MLA architecture enables efficient storage)
- **Granularity:** 64-token blocks minimum
- **TTL:** Hours to days (automatic cleanup when unused)
- **No configuration required**

### Pricing (per 1M tokens, USD)

| Model             | Cache Hit | Cache Miss | Output |
| ----------------- | --------- | ---------- | ------ |
| deepseek-chat     | $0.028    | $0.28      | $0.42  |
| deepseek-reasoner | $0.028    | $0.28      | $0.42  |

**Cache hit = 10x cheaper than cache miss**

### Multi-turn Conversation Behavior

- **Extremely high benefit** - Designed specifically for this use case
- Each turn reuses entire conversation prefix from cache
- Second turn onwards can achieve 90%+ cache hit rate
- 128K prompt with high reference: TTFT drops from 13s to 500ms

### Parallel API Calls

- **High benefit** - Automatic routing handles parallel requests
- No waiting for first response (unlike Anthropic)
- Best-effort cache hits (not guaranteed 100%)

### Subagent Patterns

- **Very high benefit** - Each user's cache is isolated
- Subagents within same API key share cache namespace
- Few-shot learning across subagents: share example prefix

### Response Fields

```json
{
  "usage": {
    "prompt_cache_hit_tokens": 50000,
    "prompt_cache_miss_tokens": 100
  }
}
```

### Optimization Strategies

1. Structure prompts with common prefix: system + few-shot examples
2. Multi-turn: preserve message history exactly
3. Data analysis: same document prefix, different queries
4. Code analysis: same repo context, different questions

### Key Advantage

DeepSeek's MLA (Multi-Head Latent Attention) architecture reduces KV cache size significantly, enabling cost-effective disk storage. First provider to implement production disk caching at scale.

---

## 5. OpenAI Prompt Caching

### Mechanism

- **Type:** Automatic prefix matching
- **Routing:** Hash of first ~256 tokens routes to same machine
- **Cache Key:** Prefix hash + optional `prompt_cache_key` parameter
- **Min Tokens:** 1024
- **TTL:** In-memory 5-10 min (up to 1 hour), Extended 24 hours

### Pricing

| Operation   | Cost                              |
| ----------- | --------------------------------- |
| Cache Write | Free                              |
| Cache Read  | 0.25-0.5x input (model dependent) |

### Extended Cache Retention

- Available on GPT-5.x, GPT-4.1
- Set `prompt_cache_retention: "24h"` in request
- KV tensors stored in GPU-local storage
- Not ZDR-eligible (derived from customer content)

### Multi-turn Conversation

- **High benefit** - Automatic caching of conversation prefix
- No configuration needed
- Works across all messages, tools, structured outputs

### Parallel API Calls

- **High benefit** - Automatic routing by prefix hash
- `prompt_cache_key` parameter for explicit routing control
- Overflow handling: >15 req/min per prefix may route to multiple machines

### Subagent Patterns

- **High benefit** - Caches not shared between organizations
- Within org: shared cache by default
- Use `prompt_cache_key` to partition cache by subagent if needed

### Implementation

```python
response = client.responses.create(
    model="gpt-5.1",
    prompt_cache_retention="24h",  # Optional: extended retention
    prompt_cache_key="my-prefix-key",  # Optional: routing hint
    messages=[...]
)
```

---

## Comparison: RLM Partition+Map Pattern

For the RLM (Recursive Language Model) pattern where you partition data and map queries in parallel:

| Provider          | RLM Suitability   | Notes                                              |
| ----------------- | ----------------- | -------------------------------------------------- |
| DeepSeek          | Excellent         | Automatic, no waiting, 90% cheaper on hits         |
| OpenAI            | Excellent         | Automatic routing, use `prompt_cache_key`          |
| Gemini (Explicit) | Excellent         | Create named cache, reference from all partitions  |
| Gemini (Implicit) | Good              | Works automatically if prefixes align              |
| Anthropic         | Poor for parallel | Must wait for first response before parallel calls |

### Recommended Pattern for RLM

**Option 1: DeepSeek or OpenAI (easiest)**

```python
# Just send all parallel requests - caching is automatic
results = await asyncio.gather(*[
    call_llm(shared_prefix + partition_data[i])
    for i in range(num_partitions)
])
```

**Option 2: Gemini Explicit (most control)**

```python
# Create cache with shared context
cache = create_cache(system_prompt + shared_context)

# Parallel requests all reference same cache
results = await asyncio.gather(*[
    call_llm(cache_name=cache.name, query=partition_data[i])
    for i in range(num_partitions)
])
```

**Option 3: Anthropic (requires sequencing)**

```python
# Send first request and wait for stream to start
first = await call_llm(shared_prefix + partition_data[0], stream=True)
await first.wait_for_first_token()

# Now send remaining in parallel
rest = await asyncio.gather(*[
    call_llm(shared_prefix + partition_data[i])
    for i in range(1, num_partitions)
])
```

---

## Subagent Architecture Recommendations

### For Shared Context Subagents

1. **Best:** DeepSeek or Gemini Explicit
   - DeepSeek: Automatic, no coordination needed
   - Gemini: Explicit cache guarantees availability

2. **Good:** OpenAI
   - Automatic caching works well
   - Use `prompt_cache_key` for routing control

3. **Requires Care:** Anthropic
   - Sequential initialization required for cache warmup
   - Once warm, subsequent calls benefit

### For Independent Subagents

- All providers work equally well
- Each subagent manages its own cache lifecycle
- No cross-agent cache sharing needed

### Cost Optimization Summary

| Scenario                   | Best Provider   | Expected Savings  |
| -------------------------- | --------------- | ----------------- |
| Long multi-turn chat       | DeepSeek        | 80-90% input cost |
| Parallel document analysis | Gemini Explicit | 75% input cost    |
| Agent with tool calls      | Anthropic       | 50-70% input cost |
| High-throughput chat       | OpenAI          | 50-75% input cost |

---

## Sources

- Anthropic Prompt Caching: https://platform.claude.com/docs/en/build-with-claude/prompt-caching
- OpenRouter Caching: https://openrouter.ai/docs/features/prompt-caching
- Google Gemini Caching: https://ai.google.dev/gemini-api/docs/caching
- DeepSeek Caching: https://api-docs.deepseek.com/guides/kv_cache
- OpenAI Caching: https://platform.openai.com/docs/guides/prompt-caching
