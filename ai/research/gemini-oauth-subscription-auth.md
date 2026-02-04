# Gemini OAuth Subscription Authentication Research

Research on how projects implement OAuth to access Gemini models with a subscription (not API keys).

## Summary

Three distinct OAuth configurations are used by different projects:

1. **Gemini CLI** (Google's official) - Uses Gemini Code Assist OAuth
2. **Antigravity plugins** (opencode-antigravity-auth) - Uses Google Cloud Code API OAuth
3. **pi-mono** - Uses the same credentials as Gemini CLI

## 1. Gemini CLI (Official Google)

Source: `google-gemini/gemini-cli/packages/core/src/code_assist/oauth2.ts`

### OAuth Configuration

| Parameter     | Value                                                                      |
| ------------- | -------------------------------------------------------------------------- |
| Client ID     | `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com` |
| Client Secret | `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`                                      |
| Scopes        | `https://www.googleapis.com/auth/cloud-platform`                           |
|               | `https://www.googleapis.com/auth/userinfo.email`                           |
|               | `https://www.googleapis.com/auth/userinfo.profile`                         |

### OAuth Endpoints

| Purpose            | URL                                                                    |
| ------------------ | ---------------------------------------------------------------------- |
| User Code Redirect | `https://codeassist.google.com/authcode`                               |
| Local Callback     | `http://127.0.0.1:{port}/oauth2callback`                               |
| Success Redirect   | `https://developers.google.com/gemini-code-assist/auth_success_gemini` |
| Failure Redirect   | `https://developers.google.com/gemini-code-assist/auth_failure_gemini` |
| User Info          | `https://www.googleapis.com/oauth2/v2/userinfo`                        |

### API Endpoint

```
https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse
```

### Auth Flow

- OAuth 2.0 Authorization Code flow with PKCE
- Access type: `offline` (enables refresh tokens)
- State parameter: Cryptographic random for CSRF protection

## 2. Antigravity Plugins (opencode-antigravity-auth / opencode-google-antigravity-auth)

Source: `firdyfirdy/antigravity-auth/src/antigravity_auth/constants.py`

### OAuth Configuration

| Parameter     | Value                                                                       |
| ------------- | --------------------------------------------------------------------------- |
| Client ID     | `1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com` |
| Client Secret | `GOCSPX-REDACTED`                                                          |
| Scopes        | `https://www.googleapis.com/auth/cloud-platform`                            |
|               | `https://www.googleapis.com/auth/userinfo.email`                            |
|               | `https://www.googleapis.com/auth/userinfo.profile`                          |
|               | Cloud Compute Log (internal)                                                |
|               | Experiments/configs (internal)                                              |

### API Endpoints (with fallback)

| Priority        | Endpoint                                               |
| --------------- | ------------------------------------------------------ |
| 1 (Development) | `https://daily-cloudcode-pa.sandbox.googleapis.com`    |
| 2 (Staging)     | `https://autopush-cloudcode-pa.sandbox.googleapis.com` |
| 3 (Production)  | `https://cloudcode-pa.googleapis.com`                  |

### Alternative Endpoints (from DeepWiki)

| Priority       | Endpoint                                                              |
| -------------- | --------------------------------------------------------------------- |
| 1 (Daily)      | `https://codeassist-pa.clients6.google.com/codeassist-pa-daily/v1`    |
| 2 (Autopush)   | `https://codeassist-pa.clients6.google.com/codeassist-pa-autopush/v1` |
| 3 (Production) | `https://codeassist.googleapis.com/v1`                                |

### Auth Flow

- OAuth 2.0 Authorization Code flow with PKCE
- S256 (SHA-256) challenge method
- Local callback: `http://localhost:36742/oauth-callback` or port 50327
- Prompt: `consent` (forces consent screen)
- Access type: `offline`

## 3. pi-mono (badlogic)

pi-mono uses the **same credentials as Gemini CLI** for Google Gemini CLI authentication.

Source: `badlogic/pi-mono/packages/ai/README.md` and provider implementation

### Configuration

Uses the Gemini CLI OAuth flow with:

- Same client ID/secret as Gemini CLI
- `GOOGLE_CLOUD_PROJECT` environment variable for project binding
- `loginGeminiCli()` function or CLI: `npx @mariozechner/pi-ai login google-gemini-cli`

### API Endpoints

| Purpose      | Endpoint                                            |
| ------------ | --------------------------------------------------- |
| Production   | `https://cloudcode-pa.googleapis.com`               |
| Sandbox      | `https://daily-cloudcode-pa.sandbox.googleapis.com` |
| Request Path | `/v1internal:streamGenerateContent?alt=sse`         |

## Request Format

All implementations use the same request format once authenticated:

```http
POST {endpoint}/v1internal:streamGenerateContent?alt=sse
Authorization: Bearer {access_token}
Content-Type: application/json
x-goog-user-project: {project_id}

{
  "model": "models/gemini-2.5-pro",
  "contents": [...],
  "generationConfig": {...}
}
```

## Token Storage

| Project     | Storage Location                               |
| ----------- | ---------------------------------------------- |
| Gemini CLI  | `~/.gemini/` or `~/.config/gemini-cli/`        |
| Antigravity | `~/.config/opencode/antigravity-accounts.json` |
| pi-mono     | Internal provider state                        |

## Important Notes

1. **These are internal Google credentials** - They may change or be revoked
2. **Subscription required** - Credentials use your Google subscription quota, not API billing
3. **Project binding** - May require `GOOGLE_CLOUD_PROJECT` for organizational accounts
4. **Deprecation risk** - Permission `cloudaicompanion.companions.generateChat` deprecated Feb 1, 2026

## Implementation Recommendation

For ion, consider:

1. **Reuse Gemini CLI credentials** - Most stable, official Google product
2. **Implement PKCE flow** - Required for security
3. **Store tokens securely** - Use system keychain when possible
4. **Support refresh tokens** - Avoid repeated auth flows
5. **Handle endpoint fallback** - Production may have issues, sandbox can help

## References

- https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/code_assist/oauth2.ts
- https://github.com/firdyfirdy/antigravity-auth
- https://github.com/NoeFabris/opencode-antigravity-auth
- https://github.com/shekohex/opencode-google-antigravity-auth
- https://github.com/badlogic/pi-mono/blob/main/packages/ai/README.md
- https://deepwiki.com/shekohex/opencode-google-antigravity-auth/4-authentication-system
- https://ai.google.dev/gemini-api/docs/oauth

---

_Research date: 2026-02-01_
