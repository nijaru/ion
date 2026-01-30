# OAuth Implementations in CLI Agents (2026)

Research on OAuth 2.0 PKCE implementations in Codex CLI (Rust), Gemini CLI (TypeScript), pi-mono (TypeScript), and OpenCode plugins.

## Summary

| CLI        | Language   | Client ID                                                                  | Token Storage                       | Callback Port |
| ---------- | ---------- | -------------------------------------------------------------------------- | ----------------------------------- | ------------- |
| Codex CLI  | Rust       | `app_EMoamEEZ73f0CkXaXp7hrann`                                             | `~/.codex/auth.json`                | 1455          |
| Gemini CLI | TypeScript | `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com` | `~/.gemini/tokens.json`             | Dynamic       |
| pi-mono    | TypeScript | (per provider)                                                             | `~/.pi/agent/auth.json`             | Dynamic       |
| OpenCode   | Go/TS      | (uses Codex client ID)                                                     | `~/.local/share/opencode/auth.json` | 1455          |

## Codex CLI (Rust)

**Source:** `codex-rs/login/src/server.rs`

### Endpoints

| Endpoint      | URL                                       |
| ------------- | ----------------------------------------- |
| Authorization | `https://auth.openai.com/oauth/authorize` |
| Token         | `https://auth.openai.com/oauth/token`     |
| Client ID     | `app_EMoamEEZ73f0CkXaXp7hrann`            |

### PKCE Flow

```rust
// 1. Generate PKCE codes
pub struct PkceCodes {
    pub code_challenge: String,
    pub code_verifier: String,
}

// 2. Generate state for CSRF protection
fn generate_state() -> String {
    // 32 bytes, base64url encoded
}

// 3. Build authorization URL
let auth_url = format!(
    "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
    auth_endpoint, client_id, redirect_uri,
    "openid profile email offline_access",
    state, pkce.code_challenge
);

// 4. Exchange code for tokens
pub async fn exchange_code_for_tokens(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> io::Result<ExchangedTokens>
```

### Local Callback Server

- Uses `tiny_http` crate
- Binds to `127.0.0.1:1455` (fixed port)
- Retry logic: 10 attempts with 200ms delays
- Sends cancel request to stale servers on `AddrInUse`
- Three endpoints: `/auth/callback`, `/success`, `/cancel`
- Uses `Connection: close` header to prevent socket reuse

### Token Storage

```json
// ~/.codex/auth.json
{
  "auth_mode": "chatgpt",
  "api_key": "...",
  "access_token": "...",
  "refresh_token": "...",
  "id_token": "...",
  "chatgpt_account_id": "..."
}
```

### Browser Launching

```rust
if open_browser {
    webbrowser::open(&auth_url)?;
}
```

### Device Code Flow (Headless)

For SSH, containers, CI environments:

1. Run `codex login --device-auth`
2. Displays URL and one-time code (e.g., ABCD-EFGH-IJKL)
3. User visits URL in any browser
4. Enter code to complete auth

### Key Structs

```rust
pub struct ServerOptions {
    pub codex_home: PathBuf,
    pub client_id: String,
    pub issuer: String,  // defaults to https://auth.openai.com
    pub port: u16,
    pub open_browser: bool,
    pub force_state: Option<String>,
    pub forced_chatgpt_workspace_id: Option<String>,
    pub cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
}

pub struct LoginServer {
    pub auth_url: String,
    pub actual_port: u16,
    server_handle: tokio::task::JoinHandle<io::Result<()>>,
    shutdown_handle: ShutdownHandle,
}
```

## Gemini CLI (TypeScript)

**Source:** `packages/core/src/code_assist/oauth2.ts`

### Endpoints

| Endpoint      | URL                                                                        |
| ------------- | -------------------------------------------------------------------------- |
| Authorization | `https://accounts.google.com/o/oauth2/v2/auth`                             |
| Token         | Google OAuth2 token endpoint                                               |
| Client ID     | `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com` |
| Client Secret | `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` (safe for installed apps)            |
| API Endpoint  | `https://cloudcode-pa.googleapis.com/v1internal`                           |

### Scopes

```
https://www.googleapis.com/auth/cloud-platform
https://www.googleapis.com/auth/userinfo.email
https://www.googleapis.com/auth/userinfo.profile
```

### PKCE Flow

```typescript
import { OAuth2Client, CodeChallengeMethod } from "google-auth-library";

// 1. Generate PKCE codes
const codeVerifier = await client.generateCodeVerifierAsync();
const state = crypto.randomBytes(32).toString("hex");

// 2. Build authorization URL
const authUrl = client.generateAuthUrl({
  redirect_uri: redirectUri,
  access_type: "offline",
  scope: OAUTH_SCOPE,
  code_challenge_method: CodeChallengeMethod.S256,
  code_challenge: codeVerifier.codeChallenge,
  state,
});

// 3. Exchange code
const { tokens } = await client.getToken({
  code: authCode,
  codeVerifier: codeVerifier.codeVerifier,
  redirect_uri: redirectUri,
});
```

### Dual Authentication Paths

**Web Browser Flow (`authWithWeb`):**

- Creates HTTP server on available port (dynamic)
- Redirect URI: `http://127.0.0.1:{port}/oauth2callback`
- Launches browser via `open` package
- 5-minute timeout
- SIGINT handler for cancellation

**User Code Flow (`authWithUserCode`):**

- No browser required
- Redirect URI: `https://codeassist.google.com/authcode`
- User visits URL, enters code via CLI

### Token Storage

```json
// ~/.gemini/tokens.json (restricted permissions)
{
  "access_token": "...",
  "refresh_token": "...",
  "expiry_date": 1234567890000
}
```

Also supports encrypted storage via `MCPOAuthTokenStorage` in system keychain.

### Browser Launching

```typescript
import open from "open";
await open(authUrl);
// Fallback: set NO_BROWSER env var, display URL for manual copy
```

### Environment Variables

- `OAUTH_CALLBACK_PORT` - Override callback port
- `OAUTH_CALLBACK_IP` - Override bind address (e.g., `0.0.0.0` for Docker)
- `NO_BROWSER` - Disable auto browser launch

## pi-mono (TypeScript)

**Source:** `packages/ai/src/auth.ts`, `packages/coding-agent/src/core/auth-storage.ts`

### Supported Providers

- `anthropic`
- `openai-codex`
- `github-copilot`
- `google-gemini-cli`
- `google-antigravity`

### Token Storage

```json
// ~/.pi/agent/auth.json
{
  "anthropic": {
    "type": "api_key",
    "key": "sk-ant-..."
  },
  "openai-codex": {
    "type": "oauth",
    "refresh": "rt_...",
    "access": "eyJhbGc...",
    "expires": 1761735358000
  }
}
```

### OAuth Credentials Interface

```typescript
interface OAuthCredentials {
  refresh: string; // Refresh token
  access: string; // Access token
  expires: number; // Expiration timestamp (ms)
}
```

### Auth Discovery

```typescript
// Priority: auth.json > environment variables
const authStorage = discoverAuthStorage();
// Reads ~/.pi/agent/auth.json
// Falls back to ANTHROPIC_API_KEY, OPENAI_API_KEY, etc.
```

### Login Callbacks

```typescript
interface AuthCallbacks {
  onAuth: (params: { url: string }) => void; // Browser OAuth
  onDeviceCode: (params: { userCode: string; verificationUri: string }) => void;
  onPrompt: (params: { message: string }) => void; // Manual token entry
}
```

## OpenCode Plugins

Uses same endpoints/client ID as Codex CLI.

### Token Storage

```json
// ~/.local/share/opencode/auth.json
{
  "openai-codex": {
    "type": "oauth",
    "refresh": "rt_...",
    "access": "eyJhbGc...",
    "expires": 1761735358000
  }
}
```

## Implementation Patterns

### PKCE Generation (Rust)

```rust
use sha2::{Sha256, Digest};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;

fn generate_pkce() -> (String, String) {
    // Generate 32 random bytes
    let mut verifier_bytes = [0u8; 32];
    rand::rng().fill(&mut verifier_bytes);

    // Base64url encode for verifier (43-128 chars)
    let code_verifier = URL_SAFE_NO_PAD.encode(&verifier_bytes);

    // SHA256 hash, then base64url encode for challenge
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

    (code_verifier, code_challenge)
}
```

### State Generation (CSRF Protection)

```rust
fn generate_state() -> String {
    let mut state_bytes = [0u8; 32];
    rand::rng().fill(&mut state_bytes);
    URL_SAFE_NO_PAD.encode(&state_bytes)
}
```

### Local Callback Server (Rust with tiny_http)

```rust
use tiny_http::{Server, Response};

fn run_callback_server(port: u16) -> Result<String> {
    let server = Server::http(format!("127.0.0.1:{}", port))?;

    for request in server.incoming_requests() {
        let url = request.url();
        if url.starts_with("/auth/callback") {
            // Parse query params
            let params: HashMap<_, _> = url::Url::parse(&format!("http://localhost{}", url))?
                .query_pairs()
                .collect();

            // Validate state
            if params.get("state") != Some(&expected_state) {
                // CSRF attack, reject
            }

            // Extract authorization code
            let code = params.get("code").ok_or("missing code")?;

            // Send success response
            let response = Response::from_string("Login successful! You can close this tab.");
            request.respond(response)?;

            return Ok(code.to_string());
        }
    }
    Err("No callback received")
}
```

### Token Refresh

```rust
async fn refresh_token_if_needed(credentials: &mut OAuthCredentials) -> Result<()> {
    let now = Utc::now().timestamp_millis() as u64;
    let refresh_threshold = 5 * 60 * 1000; // 5 minutes

    if credentials.expires_at.saturating_sub(now) < refresh_threshold {
        let response = reqwest::Client::new()
            .post(TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &credentials.refresh_token),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await?;

        let tokens: TokenResponse = response.json().await?;
        credentials.access_token = tokens.access_token;
        credentials.expires_at = now + tokens.expires_in * 1000;
        // Save updated credentials
    }
    Ok(())
}
```

## Recommended Dependencies (Rust)

```toml
[dependencies]
oauth2 = "5"           # OAuth 2.0 client with PKCE
tiny_http = "0.12"     # Lightweight callback server
open = "5"             # Cross-platform browser launch
reqwest = { version = "0.12", features = ["json"] }
sha2 = "0.10"          # PKCE challenge hashing
base64 = "0.22"        # Base64url encoding
rand = "0.9"           # Random generation
serde = { version = "1", features = ["derive"] }
serde_json = "1"       # Token storage
url = "2"              # URL parsing
```

## Security Considerations

1. **PKCE Required**: Prevents authorization code interception
2. **State Validation**: CSRF protection via random state parameter
3. **No Redirects**: Configure HTTP client with `redirect::Policy::none()` to prevent SSRF
4. **Connection Close**: Use `Connection: close` header to prevent socket reuse issues
5. **Token Storage**: Start with file-based (like Codex), consider keychain for security

## Sources

- [OpenAI Codex Auth Docs](https://developers.openai.com/codex/auth/)
- [Gemini CLI Authentication](https://deepwiki.com/google-gemini/gemini-cli/2.2-authentication)
- [pi-mono GitHub](https://github.com/badlogic/pi-mono)
- [OpenCode Codex Auth Plugin](https://github.com/numman-ali/opencode-openai-codex-auth)
- [oauth2-rs crate](https://docs.rs/oauth2/latest/oauth2/)
