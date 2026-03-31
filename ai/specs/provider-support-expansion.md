# Provider Support Expansion

## Goal

Make ion easy to connect to the providers people already use:
- direct providers
- router providers
- OpenAI-compatible endpoints
- local endpoints

Do this with minimal user configuration and clear UX.

## Current state

Ion currently exposes a provider catalog with:
- `anthropic`
- `openai`
- `openrouter`
- `gemini`
- `ollama`
- `huggingface`
- `together`
- `deepseek`
- `groq`
- `fireworks`
- `mistral`
- `xai`
- `cerebras`
- `moonshot`
- `zai`
- `openai-compatible`
- `local-api`

That is broader than the original native set, but the survey below shows a few auth/endpoint families still deserve explicit documentation.

## Survey Snapshot

### API-key providers with OpenAI-compatible surfaces

| Provider | Auth shape | Endpoint family | Ion status |
| --- | --- | --- | --- |
| `openai` | OpenAI API key | Direct OpenAI API | Native |
| `openrouter` | OpenRouter API key | OpenAI-compatible router | Native |
| `huggingface` | HF token | Router with OpenAI-compatible chat endpoint | Native |
| `together` | API key | OpenAI-compatible | Native |
| `deepseek` | API key | OpenAI-compatible | Native |
| `groq` | API key | OpenAI-compatible | Native |
| `fireworks` | API key | OpenAI-compatible, plus Anthropic-compatible Messages API | Native |
| `mistral` | API key | Direct Mistral API; also works through OpenAI-compatible gateways and self-hosted OpenAI-like servers | Native |
| `xai` | API key | OpenAI-compatible | Native |
| `cerebras` | API key | OpenAI-compatible | Native |
| `moonshot` | Bearer token / API key | OpenAI-compatible | Native |
| `zai` | API key | OpenAI-compatible and Anthropic-compatible | Native |

### Platform-auth providers

| Provider | Auth shape | Endpoint family | Ion status |
| --- | --- | --- | --- |
| `anthropic` | Anthropic API key | Direct Anthropic API | Native |
| `gemini` | Google API key / Google Cloud auth | Direct Gemini API | Native |
| `vertex` | Google Cloud auth / access token | Vertex AI Gemini endpoint | Not yet modeled |
| `bedrock` | AWS credentials or Bedrock API key | Bedrock runtime / OpenAI-compatible model inference | Not yet modeled |

### Local and custom endpoints

| Provider | Auth shape | Endpoint family | Ion status |
| --- | --- | --- | --- |
| `ollama` | Local/no auth | Local OpenAI-compatible endpoint | Native |
| `openai-compatible` | User-supplied API key / token | Custom OpenAI-compatible endpoint | Native |
| `local-api` | Local/no auth or user-supplied token | Local/custom OpenAI-compatible endpoint | Native |

## Interpretation

- Most of the providers Ion cares about collapse into a handful of runtime families: direct API, OpenAI-compatible, Anthropic-compatible, Gemini/Google auth, and local/custom endpoints.
- `Fireworks` and `Z.ai` are notable because they document both OpenAI-compatible and Anthropic-compatible surfaces.
- `Hugging Face` is a router, not a direct model host, so its auth and routing model deserves separate UX treatment.
- `Bedrock` and `Vertex` are cloud-platform cases, not simple API-key providers, so they should stay explicit if Ion ever models them natively.

## Sources

- [Hugging Face Inference Providers](https://huggingface.co/inference-api)
- [Together AI OpenAI Compatibility](https://docs.together.ai/docs/openai-api-compatibility)
- [DeepSeek API docs](https://api-docs.deepseek.com/)
- [Groq OpenAI Compatibility](https://console.groq.com/docs/openai)
- [Fireworks OpenAI Compatibility](https://docs.fireworks.ai/tools-sdks/openai-compatibility)
- [Fireworks Anthropic Compatibility](https://docs.fireworks.ai/tools-sdks/anthropic-compatibility)
- [xAI code editor setup](https://docs.x.ai/docs/guides/use-with-code-editors)
- [Cerebras OpenAI Compatibility](https://inference-docs.cerebras.ai/resources/openai)
- [Z.AI API reference](https://docs.z.ai/api-reference/introduction)
- [Z.AI Anthropic-compatible setup](https://docs.z.ai/devpack/quick-start)
- [Amazon Bedrock OpenAI APIs](https://docs.aws.amazon.com/en_us/bedrock/latest/userguide/bedrock-mantle.html)
- [Amazon Bedrock OpenAI compatibility](https://docs.aws.amazon.com/bedrock/latest/userguide/model-parameters-openai.html)
- [Vertex AI OpenAI compatibility](https://cloud.google.com/vertex-ai/generative-ai/docs/start/openai)

## Key findings

### 1. ion is currently hardcoded, not provider-agnostic enough

Current provider routing and picker logic are hardcoded in:
- `cmd/ion/selection.go`
- `internal/app/picker.go`
- `internal/backend/canto/backend.go`

This is fine for a small initial set, but it will not scale well if ion wants broad provider support.

### 2. canto already gives ion a better path than per-provider runtimes

The important detail in canto is that the existing provider adapters already support custom API endpoints:

- OpenAI provider accepts `APIEndpoint`
- Anthropic provider accepts `APIEndpoint`
- Gemini provider accepts `APIEndpoint`
- OpenRouter provider accepts `APIEndpoint`
- Ollama provider accepts `APIEndpoint`

That means ion can support many providers through a smaller number of protocol families:

- OpenAI-compatible
- Anthropic-compatible
- Gemini-compatible
- OpenRouter
- Ollama/local

This is the main scalability lever.

### 3. pi and opencode both point toward provider-agnostic UX

Useful patterns from pi/opencode:
- provider support is treated as a product surface, not a tiny allowlist
- custom/base-url-style support matters
- local models matter
- auth method and provider concept must be separated cleanly

## Recommended provider model

### Tier 1: first-class direct providers

Keep explicit support for:
- `anthropic`
- `openai`
- `openrouter`
- `gemini`
- `ollama`

Add likely high-value direct providers:
- `huggingface`
- `together`
- `deepseek`
- `zai`
- `moonshot`
- `groq`
- `fireworks`
- `mistral`
- `xai`
- `cerebras`

### Tier 2: compatibility families

Instead of bespoke logic for every vendor, ion should support compatibility families:

1. `openai-compatible`
   - custom base URL
   - custom API key env var or explicit token field
   - works for many vendors and self-hosted gateways

2. `anthropic-compatible`
   - for vendors that mirror Anthropic’s API surface

3. `gemini-compatible`
   - less common, but should remain possible

4. `local-openai`
   - LM Studio
   - vLLM
   - llama.cpp servers exposing OpenAI-style APIs
   - other local gateways

### Tier 3: cloud-host routing providers

Support router-like providers explicitly when they have real user demand:
- `openrouter`
- `huggingface`

Do not treat those as the same thing as a direct provider.

## Auth model

The UX needs to separate:
- provider
- endpoint family
- auth method

Likely auth buckets:
- API key / bearer token
- local / no auth
- OAuth / subscription bridge via ACP

For most of the providers relevant here, native direct support is API key or token based.
OAuth still belongs primarily to ACP/subscription flows.

## UX direction

### Picker

Provider picker should eventually group providers by kind:
- Direct APIs
- Routers
- Local
- Custom endpoints

And each row should communicate one of:
- `Ready`
- `Missing • set <ENV_VAR>`
- `Local`
- `Needs setup`

### Configuration

The less manual config the better, but broad support requires some structure.

Recommended minimal config surface:
- provider id
- model id
- optional endpoint override
- optional auth env var name
- optional extra headers for router/custom providers

Default cases should require no extra setup beyond the expected env var.

### Custom endpoints

ion should support:
- custom OpenAI-compatible endpoint
- custom local endpoint

without forcing users to pretend they are `openai` or `ollama` if that creates confusing UX.

## Implementation direction

### Phase 1

Add a provider catalog in ion describing:
- display name
- family
- auth kind
- default endpoint
- default env var
- whether model listing is supported

Drive:
- picker rows
- backend selection
- model listing
- startup validation

from that catalog instead of hardcoded switch statements scattered across files.

### Phase 2

Add:
- `openai-compatible`
- `huggingface`
- `together`
- `deepseek`
- `groq`
- `fireworks`
- `mistral`
- `xai`

### Phase 3

Add:
- `zai`
- `moonshot`
- `cerebras`
- `bedrock`
- `vertex`
- broader local endpoint presets

## Non-goals

- do not mix subscription/OAuth providers into the native picker until the UX is real
- do not add a huge YAML-style provider config system
- do not make catwalk or models.dev mandatory runtime dependencies

## Related tasks

- `tk-02au` — Provider support expansion
- `tk-ekao` — Provider auth and endpoint survey
