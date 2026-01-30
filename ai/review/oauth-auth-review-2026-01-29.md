# OAuth Auth Module Review

**Date:** 2026-01-29
**Reviewer:** Claude (reviewer agent)
**Files:** `src/auth/` (all), `src/provider/client.rs`

## Summary

OAuth implementation is solid overall. PKCE flow is correct, token storage is secure, and error handling is good. Found one ERROR-level bug (token refresh not used during API calls), two WARNs, and some NITs.

## Findings

### ERROR: Token refresh not called during API usage

**File:** `src/provider/client.rs:38-55`

The `Client::from_provider()` method directly reads from storage without checking if tokens need refresh. The `auth::get_credentials()` function exists and handles refresh, but it's never called.

```rust
// Current (broken):
pub fn from_provider(provider: Provider) -> Result<Self, Error> {
    if let Some(oauth_provider) = provider.oauth_provider() {
        let storage = auth::AuthStorage::new()?;
        let creds = storage.load(oauth_provider)?  // <-- No refresh check!
            .ok_or_else(...)?;
        return Self::new(provider, creds.token());
    }
    ...
}
```

**Impact:** After ~1 hour (typical token lifetime), OAuth tokens expire and API calls fail even though a valid refresh token exists. Users must manually re-login.

**Fix:** Use `auth::get_credentials()` which handles refresh:

```rust
pub async fn from_provider(provider: Provider) -> Result<Self, Error> {
    if let Some(oauth_provider) = provider.oauth_provider() {
        let creds = auth::get_credentials(oauth_provider).await?
            .ok_or_else(|| Error::MissingApiKey {
                backend: provider.name().to_string(),
                env_vars: vec![format!(
                    "Run 'ion login {}' to authenticate",
                    oauth_provider.storage_key()
                )],
            })?;
        return Self::new(provider, creds.token());
    }
    ...
}
```

**Note:** This requires making `from_provider` async, which ripples to callers.

**Confidence:** 95%

---

### WARN: No expiry validation in `is_logged_in`

**File:** `src/auth/mod.rs:110-115`

```rust
pub fn is_logged_in(provider: OAuthProvider) -> bool {
    AuthStorage::new()
        .ok()
        .and_then(|s| s.load(provider).ok().flatten())
        .is_some()  // <-- Only checks if credentials exist, not if valid
}
```

**Impact:** Provider selector shows OAuth providers as "logged in" even when tokens are expired and no refresh token exists (can't recover).

**Fix:** Add expiry check, accounting for refresh capability:

```rust
pub fn is_logged_in(provider: OAuthProvider) -> bool {
    AuthStorage::new()
        .ok()
        .and_then(|s| s.load(provider).ok().flatten())
        .map(|c| match c {
            Credentials::ApiKey { .. } => true,
            Credentials::OAuth(tokens) => {
                // Valid if not expired, or expired but has refresh token
                !tokens.is_expired() || tokens.refresh_token.is_some()
            }
        })
        .unwrap_or(false)
}
```

**Confidence:** 85%

---

### WARN: Silent failure when refresh token missing

**File:** `src/auth/mod.rs:90-104`

```rust
if let Credentials::OAuth(ref tokens) = creds
    && tokens.needs_refresh()
    && let Some(ref refresh_token) = tokens.refresh_token  // <-- Silent skip if None
{
    // refresh happens
}
Ok(Some(creds))  // <-- Returns expired token!
```

**Impact:** If tokens need refresh but no refresh token exists, returns the expired token anyway. API call will fail with confusing error.

**Fix:** Return an explicit error when token is expired and can't be refreshed:

```rust
if let Credentials::OAuth(ref tokens) = creds && tokens.needs_refresh() {
    match tokens.refresh_token {
        Some(ref refresh_token) => {
            // refresh happens...
        }
        None => {
            anyhow::bail!(
                "OAuth token expired and no refresh token available. \
                 Please run 'ion login {}' again.",
                provider.storage_key()
            );
        }
    }
}
```

**Confidence:** 90%

---

### NIT: `needs_refresh()` returns false when no expiry

**File:** `src/auth/storage.rs:31-42`

```rust
pub fn needs_refresh(&self) -> bool {
    let Some(expires_at) = self.expires_at else {
        return false;  // <-- Assumes token never expires
    };
    ...
}
```

**Observation:** If a provider doesn't return `expires_in`, tokens are assumed to be valid forever. This is probably fine in practice (most OAuth providers return expiry), but could lead to silent failures with unusual providers.

**Confidence:** 70%

---

### NIT: Duplicate `TokenResponse` structs

**Files:** `src/auth/openai.rs:104-110`, `src/auth/openai.rs:156-162`, `src/auth/google.rs:111-117`, `src/auth/google.rs:164-170`

Each file defines `TokenResponse` twice (in `refresh()` and `exchange_code()`). Could be a single struct defined once per module.

**Confidence:** 100% (not a bug, just cleanup)

---

## What's Good

1. **PKCE implementation** - Correct per RFC 7636:
   - 32 random bytes for verifier (43 chars base64url)
   - SHA-256 + base64url for challenge
   - S256 method specified in auth URL

2. **State parameter** - CSRF protection properly implemented:
   - Random 32-byte state generated per flow
   - Validated on callback before exchanging code

3. **Token storage security:**
   - File permissions set to 0600 on Unix
   - Tokens stored in user config dir (`~/.config/ion/auth.json`)
   - Refresh tokens preserved across refresh cycles

4. **Callback server:**
   - HTML-escapes error messages to prevent XSS
   - Validates state before processing code
   - Proper error responses with user-friendly pages

5. **Provider-specific handling:**
   - Google: Includes `client_secret` (required for installed apps)
   - Google: Uses `access_type=offline` + `prompt=consent` for refresh tokens
   - OpenAI: Uses PKCE without client_secret (public client)

## Test Coverage

Existing tests cover:

- PKCE generation and uniqueness
- State generation
- Callback parsing (success, state mismatch, error)
- Token needs_refresh logic
- Credentials serialization

Missing tests:

- Token refresh flow (would need mock HTTP)
- `is_expired()` edge cases
- `get_credentials()` refresh path

## Recommendation

**Priority 1:** Fix the `Client::from_provider()` token refresh issue (ERROR). This is a real bug that will affect users after tokens expire.

**Priority 2:** Fix `is_logged_in()` to check expiry (WARN). Minor UX issue.

**Priority 3:** Error explicitly when refresh impossible (WARN). Better UX on edge case.
