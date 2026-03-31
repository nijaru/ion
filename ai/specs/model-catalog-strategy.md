# Model Catalog Strategy

## Problem

ion needs fresh, production-safe model discovery for `/model` and the model picker.

Current constraints:
- `internal/backend/registry/models.go` historically relied on `catwalk.New()`.
- upstream `catwalk.New()` defaults to `http://localhost:8080` unless `CATWALK_URL` is set.
- that default is not acceptable for normal ion usage.
- OpenRouter changes frequently enough that stale third-party catalogs are not a sufficient source of truth.

Related input:
- `/Users/nick/github/charmbracelet/catwalk/FINDINGS.md`

## Catwalk findings summary

Catwalk is useful, but it is not currently a complete runtime solution for ion:

- runtime client:
  - `pkg/catwalk/client.go` talks to a catwalk server
  - default base URL is localhost
- fresh provider fetchers:
  - exist, but only as standalone `cmd/*/main.go` programs
  - not exposed as reusable runtime library code
- served catalog:
  - static provider/model JSON embedded into the catwalk binary
  - updated by repo-local scrapers / CI, not by ion at runtime

Conclusion:
- catwalk may still be useful as an explicit remote catalog
- catwalk is not a sufficient default runtime fetch path for ion on its own

## Working rules

- no localhost assumptions
- no hidden dependency on an external catwalk daemon
- if a provider offers an official model list endpoint, prefer that endpoint
- cache fetched model lists locally with a short TTL
- if live fetch fails, fall back to cached data when available
- if no live or cached data exists, fail clearly
- keep provider support scoped to what ion actually supports natively

## Current implementation

ion now uses direct fetchers for every native provider it exposes:

- `anthropic` → `GET https://api.anthropic.com/v1/models?limit=1000`
- `openai` → `GET https://api.openai.com/v1/models`
- `openrouter` → `GET https://openrouter.ai/api/v1/models`
- `gemini` → `GET https://generativelanguage.googleapis.com/v1beta/models`
- `ollama` → `GET <OLLAMA_HOST>/api/tags` with `OLLAMA_HOST` normalized and defaulting to `http://127.0.0.1:11434`

Metadata lookup now follows the same provider catalog path instead of using a separate catwalk-only rule for most providers.

## Provider-by-provider direction

### OpenRouter

Source of truth:
- direct OpenRouter API
- `GET https://openrouter.ai/api/v1/models`

Rationale:
- high churn
- broad model surface
- official endpoint already exists
- ion already depends on OpenRouter for live native usage

Current direction:
- direct fetch + local cache
- implemented

### OpenAI

Current direction:
- direct `GET /v1/models` + local cache
- pricing/context metadata may remain partial when the API does not expose it
- do not silently fall back to localhost catwalk

### Anthropic

Current direction:
- direct `GET /v1/models` + local cache
- use returned `max_input_tokens` when available
- do not silently fall back to localhost catwalk

### Gemini

Current direction:
- direct `GET /v1beta/models` + local cache
- filter to models that support `generateContent`
- use `inputTokenLimit` when available

### Ollama

Current direction:
- local endpoint is appropriate
- implemented via `/api/tags`

## Candidate architecture

### Preferred

Provider-specific runtime fetchers in ion:
- Anthropic: direct API
- OpenAI: direct API
- OpenRouter: direct API
- Gemini: direct API
- Ollama: local API

Layered on top of:
- local JSON cache in `~/.ion/data`
- short TTL
- fallback to cache on fetch failure

### Optional

Use catwalk only when explicitly configured:
- e.g. `CATWALK_URL`
- treat it as a remote catalog service, not a required default

### Possible later addition

Use models.dev or another curated public catalog as:
- a metadata supplement
- a fallback for providers without good runtime listing APIs

But:
- do not let this become an opaque hidden dependency
- document freshness tradeoffs clearly

## Decisions now made

1. Native providers use direct runtime fetchers first.
2. Local cache is the only automatic fallback.
3. Catwalk is explicit optional remote catalog only.
4. models.dev is not a runtime dependency today.
5. TTL remains the existing local cache TTL from ion config.

## Near-term implementation priority

1. keep direct-provider fetchers as the default path
2. use optional catwalk only when explicitly configured
3. improve partial metadata coverage provider-by-provider where official APIs do not expose pricing/context
4. revisit models.dev only if a clear metadata gap remains after that
