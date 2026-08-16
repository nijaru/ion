package config

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"
)

func TestGeneratePKCEAndState(t *testing.T) {
	v1, c1, err := GeneratePKCE()
	if err != nil {
		t.Fatalf("GeneratePKCE 1 failed: %v", err)
	}
	if len(v1) < 40 || len(c1) < 40 {
		t.Fatalf("unexpected PKCE length: verifier=%d, challenge=%d", len(v1), len(c1))
	}

	v2, c2, err := GeneratePKCE()
	if err != nil {
		t.Fatalf("GeneratePKCE 2 failed: %v", err)
	}
	if v1 == v2 || c1 == c2 {
		t.Fatal("GeneratePKCE returned identical values")
	}

	s1, err := GenerateRandomState()
	if err != nil {
		t.Fatalf("GenerateRandomState failed: %v", err)
	}
	s2, err := GenerateRandomState()
	if err != nil {
		t.Fatalf("GenerateRandomState 2 failed: %v", err)
	}
	if s1 == s2 || len(s1) < 20 {
		t.Fatal("GenerateRandomState returned weak or identical states")
	}
}

func TestRefreshOAuthCredential(t *testing.T) {
	payload := base64.RawURLEncoding.EncodeToString([]byte(`{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-new"}}`))
	accessToken := "e30." + payload + ".sig"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := r.ParseForm(); err != nil {
			t.Fatalf("parse form: %v", err)
		}
		if r.Form.Get("grant_type") != "refresh_token" || r.Form.Get("refresh_token") != "refresh-old" {
			t.Fatalf("refresh form = %v", r.Form)
		}
		_ = json.NewEncoder(w).Encode(OAuthTokens{
			AccessToken:  accessToken,
			RefreshToken: "refresh-new",
			ExpiresIn:    3600,
		})
	}))
	defer server.Close()

	tokens, err := refreshOAuthCredential(t.Context(), server.Client(), OAuthProviderConfig{
		ClientID:         "client-test",
		TokenURL:         server.URL,
		AccountIDFromJWT: true,
	}, CredentialProvider{RefreshToken: "refresh-old"})
	if err != nil {
		t.Fatalf("refresh: %v", err)
	}
	if tokens.AccessToken != accessToken || tokens.RefreshToken != "refresh-new" || tokens.AccountID != "acct-new" {
		t.Fatalf("tokens = %#v", tokens)
	}
}

func TestStartPKCEOAuthFlow(t *testing.T) {
	tempDir := t.TempDir()
	t.Setenv("HOME", tempDir)

	var receivedVerifier string
	payload := base64.RawURLEncoding.EncodeToString([]byte(`{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-test"}}`))
	accessToken := "e30." + payload + ".sig"
	tokenServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "invalid method", http.StatusMethodNotAllowed)
			return
		}
		if err := r.ParseForm(); err != nil {
			http.Error(w, "invalid form", http.StatusBadRequest)
			return
		}
		if r.Form.Get("grant_type") != "authorization_code" {
			http.Error(w, "invalid grant_type", http.StatusBadRequest)
			return
		}
		if r.Form.Get("code") != "mock-auth-code" {
			http.Error(w, "invalid code", http.StatusBadRequest)
			return
		}
		receivedVerifier = r.Form.Get("code_verifier")

		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(OAuthTokens{
			AccessToken:  accessToken,
			RefreshToken: "mock-refresh-token-67890",
			TokenType:    "Bearer",
			ExpiresIn:    3600,
		})
	}))
	defer tokenServer.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	var capturedAuthURL string
	done := make(chan struct{})

	go func() {
		defer close(done)
		tokens, err := StartPKCEOAuthFlow(ctx, StartOAuthFlowOptions{
			Provider:         "openai-codex",
			OpenBrowser:      false,
			HTTPClient:       tokenServer.Client(),
			TokenURLOverride: tokenServer.URL,
			URLCallback: func(authURL string) {
				capturedAuthURL = authURL

				// Parse callback redirect URI and state
				parsed, err := url.Parse(authURL)
				if err != nil {
					t.Errorf("failed to parse auth URL: %v", err)
					return
				}
				q := parsed.Query()
				redirectURI := q.Get("redirect_uri")
				state := q.Get("state")

				// Simulate user browser redirecting to callback
				callbackURL := fmt.Sprintf("%s?code=mock-auth-code&state=%s", redirectURI, state)
				resp, err := http.Get(callbackURL)
				if err != nil {
					t.Errorf("callback GET failed: %v", err)
					return
				}
				defer resp.Body.Close()
				if resp.StatusCode != http.StatusOK {
					t.Errorf("callback returned status: %d", resp.StatusCode)
				}
			},
		})
		if err != nil {
			t.Errorf("StartPKCEOAuthFlow failed: %v", err)
			return
		}
		if tokens.AccessToken != accessToken {
			t.Errorf("access token = %q, want test token", tokens.AccessToken)
		}
		if tokens.AccountID != "acct-test" {
			t.Errorf("account ID = %q, want acct-test", tokens.AccountID)
		}
		if tokens.RefreshToken != "mock-refresh-token-67890" {
			t.Errorf("expected refresh token 'mock-refresh-token-67890', got %q", tokens.RefreshToken)
		}
	}()

	select {
	case <-done:
	case <-time.After(4 * time.Second):
		t.Fatal("OAuth flow timed out")
	}

	if capturedAuthURL == "" || !strings.Contains(capturedAuthURL, "code_challenge=") {
		t.Fatalf("invalid captured auth URL: %q", capturedAuthURL)
	}
	if receivedVerifier == "" {
		t.Fatal("token server did not receive PKCE code_verifier")
	}

	// Verify tokens were saved to credentials file
	cred, ok := LookupOAuthTokens("openai-codex")
	if !ok {
		t.Fatal("expected LookupOAuthTokens to find saved credentials")
	}
	if cred.AccessToken != accessToken {
		t.Fatalf("saved access token = %q, want test token", cred.AccessToken)
	}
	if cred.AccountID != "acct-test" {
		t.Fatalf("saved account ID = %q, want acct-test", cred.AccountID)
	}

	// Verify LookupAPIKey also returns the OAuth access token.
	apiKey, ok := LookupAPIKey("openai-codex")
	if !ok || apiKey != accessToken {
		t.Fatalf("LookupAPIKey = %q, %v; want test token, true", apiKey, ok)
	}
}
