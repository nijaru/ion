# OAuth Subscription Support

Use ChatGPT Plus/Pro and Google AI subscriptions instead of API credits.

## Value Proposition

| Approach         | Cost            | Rate Limits          |
| ---------------- | --------------- | -------------------- |
| API Keys         | Pay per token   | High                 |
| ChatGPT Plus     | $20/month flat  | ~80 msgs/3hr         |
| ChatGPT Pro      | $200/month flat | Higher               |
| Google AI (free) | $0              | 60 req/min, 1000/day |

## Reference Implementations

### OpenAI Codex (Rust)

**OAuth 2.0 PKCE Flow:**

1. Start local server on `localhost:1455`
2. Generate `code_verifier` and `code_challenge`
3. Open browser to auth endpoint
4. User authenticates, redirected back with `authorization_code`
5. Exchange code for tokens
6. Store in `~/.codex/auth.json`

**Endpoints:**

- Client ID: `app_EMoamEEZ73f0CkXaXp7hrann`
- Auth: `https://auth.openai.com/oauth/authorize`
- Token: `https://auth.openai.com/oauth/token`

**Device Code (headless):**

- For SSH, containers, CI environments
- User visits URL, enters code manually

**Source:** `codex-rs/login/src/server.rs`

### Gemini CLI (TypeScript)

**OAuth Flow:**

- `packages/core/src/code_assist/oauth2.ts`
- Uses Google OAuth endpoints
- Tokens stored in system keychain via `MCPOAuthTokenStorage`

**Auth Types:**

- `oauth` - Google account
- `oauth-personal` - Personal Google account
- `api-key` - Gemini API key
- `vertex-ai` - GCP Vertex AI

**Source:** `packages/core/src/core/apiKeyCredentialStorage.ts`

## Implementation Plan

### Phase 1: OAuth Infrastructure

**New module:** `src/auth/`

```
src/auth/
├── mod.rs           # Auth traits and types
├── oauth.rs         # PKCE flow, browser launch
├── server.rs        # Local callback server
├── storage.rs       # Credential storage
└── device_code.rs   # Headless auth (optional)
```

**Core types:**

```rust
pub enum AuthMethod {
    ApiKey(String),
    OAuth(OAuthCredentials),
}

pub struct OAuthCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub trait CredentialStorage {
    fn load(&self, provider: &str) -> Option<AuthMethod>;
    fn save(&self, provider: &str, auth: &AuthMethod) -> Result<()>;
    fn clear(&self, provider: &str) -> Result<()>;
}
```

### Phase 2: ChatGPT OAuth

**Add to Provider enum:**

```rust
pub enum Provider {
    // Existing API providers
    Anthropic,
    OpenAI,
    Google,
    // ...

    // OAuth subscription providers
    ChatGptPlus,   // Uses OpenAI OAuth
    GoogleAi,      // Uses Google OAuth
}
```

**Login command:**

```bash
ion login chatgpt    # Opens browser for ChatGPT OAuth
ion login google     # Opens browser for Google OAuth
ion logout chatgpt   # Clears stored credentials
```

### Phase 3: Google OAuth

Same pattern as ChatGPT but with Google endpoints.

### Phase 4: Provider Selection

Auto-detect available auth:

1. Check for OAuth tokens first (free!)
2. Fall back to API keys
3. Prompt for login if neither available

## File Storage

```
~/.config/ion/
├── config.toml      # User preferences
├── auth.json        # OAuth tokens (plaintext, like Codex)
└── sessions.db      # Session history
```

Or use OS keychain for better security (like Gemini CLI).

## Security Considerations

1. **Token storage**: Start with file-based (like Codex), upgrade to keychain later
2. **PKCE required**: Prevents authorization code interception
3. **Token refresh**: Handle expired tokens gracefully
4. **Secure callback**: Validate state parameter

## Dependencies

```toml
[dependencies]
# OAuth
oauth2 = "5"           # OAuth 2.0 client
tiny_http = "0.12"     # Local callback server
open = "5"             # Open browser
# Optional: keyring for secure storage
keyring = "3"
```

## References

- [OpenAI Codex Auth Docs](https://developers.openai.com/codex/auth/)
- [Gemini CLI Auth](https://deepwiki.com/google-gemini/gemini-cli/2.2-authentication)
- [OpenCode Codex Auth Plugin](https://github.com/numman-ali/opencode-openai-codex-auth)
- [OAuth 2.0 PKCE RFC](https://datatracker.ietf.org/doc/html/rfc7636)
