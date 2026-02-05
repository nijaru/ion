# Sprint 15: Fix Gemini OAuth Subscription Support

**Goal:** Working Gemini OAuth for subscription users

**Source:** tk-f564, session investigation 2026-02-04

**Status:** Planning

## Background

OAuth flow completes but API requests fail with 404. Root cause: using wrong credentials and request formats.

### Gemini Findings (from gemini-cli source at `/Users/nick/github/google-gemini/gemini-cli`)

| Component       | Our Code                                         | Gemini CLI                                        |
| --------------- | ------------------------------------------------ | ------------------------------------------------- |
| OAuth Client ID | `1071006060591-...` (Antigravity)                | `681255809395-...` (Gemini CLI)                   |
| OAuth Secret    | `(old Antigravity secret)`                       | `(see src/auth/google.rs)`                        |
| Scopes          | 5 scopes (includes cclog, experimentsandconfigs) | 3 scopes (cloud-platform, userinfo.email/profile) |
| Model format    | `models/gemini-3-flash`                          | `gemini-3-flash` (no prefix)                      |
| Request fields  | `user_agent`, `request_id`                       | `user_prompt_id`                                  |
| Callback        | Port 51121, `/oauth-callback`                    | Dynamic port, `/oauth2callback`                   |
| Endpoint        | `cloudcode-pa.googleapis.com/v1internal` ✓       | Same (correct)                                    |

### ChatGPT Status

Deferred to separate sprint. Verify if actually broken first.

### Important Notes

- **Re-login required:** After updating, users must run `ion login gemini` (overwrites old tokens automatically)

---

## Task 0: Reproduce and document exact error

**Depends on:** none

### Description

Capture the exact HTTP error response before making changes.

### Steps

```bash
ion login gemini
RUST_LOG=debug ion 2>&1 | tee gemini-debug.log
# Send a message, capture the error
```

### Acceptance Criteria

- [ ] Exact HTTP status code documented (expected: 404)
- [ ] Response body captured
- [ ] Baseline established for comparison

---

## Task 1: Update Gemini OAuth credentials

**Depends on:** Task 0

**Files:** `src/auth/google.rs`

### Description

Replace Antigravity OAuth credentials with Gemini CLI credentials.

### Changes

```rust
// Old
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "(old Antigravity secret)";

// New (from gemini-cli/packages/core/src/code_assist/oauth2.ts)
const CLIENT_ID: &str = "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "(see src/auth/google.rs)";
```

Also update scopes to match (remove cclog, experimentsandconfigs).

### Acceptance Criteria

- [ ] Client ID matches Gemini CLI
- [ ] Client secret matches Gemini CLI
- [ ] Scopes reduced to 3 (cloud-platform, userinfo.email, userinfo.profile)
- [ ] `ion login gemini` completes successfully

---

## Task 2a: Fix model name format

**Depends on:** Task 1

**Files:** `src/provider/subscription/gemini.rs`

### Description

Remove `models/` prefix from model name in API requests.

### Changes

```rust
// Old
fn normalize_model_name(model: &str) -> String {
    // ... stripping logic ...
    format!("models/{stripped}")
}

// New
fn normalize_model_name(model: &str) -> String {
    let trimmed = model.trim();
    let stripped = trimmed.strip_prefix("models/").unwrap_or(trimmed);
    stripped.to_string()  // No prefix added
}
```

### Acceptance Criteria

- [ ] `gemini-3-flash` -> `gemini-3-flash`
- [ ] `models/gemini-3-flash` -> `gemini-3-flash`
- [ ] Test after this change - may fix 404 alone

---

## Task 2b: Update request fields

**Depends on:** Task 2a

**Files:** `src/provider/subscription/gemini.rs`

### Description

Update request struct to match Gemini CLI format.

### Changes

```rust
// Old
struct CodeAssistRequest {
    project: String,
    model: String,
    request: VertexRequest,
    user_agent: Option<String>,
    request_id: Option<String>,
}

// New (match gemini-cli/packages/core/src/code_assist/converter.ts)
struct CodeAssistRequest {
    model: String,
    project: Option<String>,
    user_prompt_id: Option<String>,
    request: VertexRequest,
}
```

Generate `user_prompt_id` as UUID or timestamp-based ID.

### Acceptance Criteria

- [ ] Request uses `user_prompt_id` field
- [ ] `user_agent` and `request_id` fields removed
- [ ] `project` is optional

---

## Task 3: Test Gemini OAuth end-to-end

**Depends on:** Task 2b

### Description

Verify complete flow works.

### Test Steps

```bash
# Clear existing auth
ion logout gemini

# Fresh login
ion login gemini

# Verify token stored
cat ~/.config/ion/auth.json | jq '.google'

# Test in TUI
ion
# Select Gemini provider (Ctrl+P)
# Select model (gemini-3-flash-preview)
# Send message: "hello"

# If errors, capture debug output
RUST_LOG=debug ion 2>&1 | tee test-output.log
```

### Acceptance Criteria

- [ ] Login completes without error
- [ ] Provider shows as authenticated (green dot)
- [ ] API request returns valid response (no 404)
- [ ] Streaming works correctly
- [ ] Multi-turn conversation works

---

## Task 4: Update documentation

**Depends on:** Task 3

**Files:** `ai/design/oauth-subscriptions.md`

### Description

Update documentation with correct credentials and working status.

### Acceptance Criteria

- [ ] Credentials section updated
- [ ] Status reflects reality (working)
- [ ] Note about re-login requirement after update
- [ ] Gemini CLI source reference added

---

## Verification

After all tasks:

```bash
# Full test
ion login gemini
ion  # Test conversation
```

## Future Work (Separate Sprint)

- Investigate ChatGPT OAuth (verify if broken first)
- Dynamic callback port (like Gemini CLI)
- Callback path alignment (`/oauth2callback` vs `/oauth-callback`)
