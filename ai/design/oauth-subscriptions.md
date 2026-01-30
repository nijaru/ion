# OAuth Subscription Support

Use ChatGPT and Gemini subscriptions instead of API credits.

## Status: Implemented

| Component       | Status |
| --------------- | ------ |
| PKCE flow       | Done   |
| Callback server | Done   |
| Token storage   | Done   |
| ChatGPT OAuth   | Done   |
| Gemini OAuth    | Done   |
| CLI commands    | Done   |
| TUI integration | Done   |

## Usage

```bash
# Login
ion login chatgpt   # Opens browser for ChatGPT OAuth
ion login gemini    # Opens browser for Google OAuth

# Logout
ion logout chatgpt
ion logout gemini
```

After login, select the provider in TUI (Ctrl+P):

- **ChatGPT** - "Sign in with ChatGPT"
- **Gemini** - "Sign in with Google"

## Value Proposition

| Provider      | Cost            | Rate Limits          |
| ------------- | --------------- | -------------------- |
| API Keys      | Pay per token   | High                 |
| ChatGPT Plus  | $20/month flat  | ~80 msgs/3hr         |
| ChatGPT Pro   | $200/month flat | Higher               |
| Gemini (free) | $0              | 60 req/min, 1000/day |

## Architecture

```
src/auth/
├── mod.rs        # Auth traits, login/logout functions
├── pkce.rs       # PKCE code generation (RFC 7636)
├── server.rs     # Local callback server (port 1455)
├── storage.rs    # Token storage (~/.config/ion/auth.json)
├── openai.rs     # ChatGPT OAuth endpoints
└── google.rs     # Gemini OAuth endpoints
```

## Provider Naming

| ID        | CLI                 | Display   | Description          |
| --------- | ------------------- | --------- | -------------------- |
| `openai`  | -                   | OpenAI    | API key access       |
| `chatgpt` | `ion login chatgpt` | ChatGPT   | Sign in with ChatGPT |
| `google`  | -                   | Google AI | API key access       |
| `gemini`  | `ion login gemini`  | Gemini    | Sign in with Google  |

## Token Storage

```json
// ~/.config/ion/auth.json
{
  "openai": {
    "type": "oauth",
    "access_token": "...",
    "refresh_token": "...",
    "expires_at": 1234567890000
  },
  "google": {
    "type": "oauth",
    "access_token": "...",
    "refresh_token": "...",
    "expires_at": 1234567890000
  }
}
```

File permissions: 0600 (user read/write only)

## OAuth Flow

1. Generate PKCE codes (verifier + challenge)
2. Generate state for CSRF protection
3. Start callback server on localhost:1455
4. Open browser to auth endpoint
5. User authenticates
6. Receive callback with authorization code
7. Exchange code for tokens (with PKCE verifier)
8. Store tokens in auth.json

## Credentials

Both use official public client IDs from their respective CLIs:

- **ChatGPT**: Codex CLI client ID (`app_EMoamEEZ73f0CkXaXp7hrann`)
- **Gemini**: Gemini CLI client ID (Google-owned)

These are installed-app credentials, safe to embed per OAuth spec.

## Future: Extensibility

3rd party OAuth providers can be added via config or plugins:

```toml
# Config approach (technical users)
[[oauth]]
id = "anthropic"
name = "Claude"
client_id = "app_xxx"
auth_url = "https://auth.anthropic.com/oauth/authorize"
token_url = "https://auth.anthropic.com/oauth/token"
scopes = ["openid", "offline_access"]
api_type = "anthropic"
```

Plugin approach: drop-in file with pre-configured provider (for non-technical users).

See tasks: tk-o0g7 (API providers), tk-a2s8 (OAuth providers)

## References

- [OpenAI Codex Auth](https://developers.openai.com/codex/auth/)
- [Gemini CLI Auth](https://deepwiki.com/google-gemini/gemini-cli/2.2-authentication)
- [OAuth 2.0 PKCE RFC](https://datatracker.ietf.org/doc/html/rfc7636)
- Research: `ai/research/oauth-implementations-2026.md`
