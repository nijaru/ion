package config

import (
	"context"
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

func TestStartPKCEOAuthFlow(t *testing.T) {
	tempDir := t.TempDir()
	t.Setenv("HOME", tempDir)

	var receivedVerifier string
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
			AccessToken:  "mock-access-token-12345",
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
			Provider:         "openai",
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
		if tokens.AccessToken != "mock-access-token-12345" {
			t.Errorf("expected access token 'mock-access-token-12345', got %q", tokens.AccessToken)
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
	cred, ok := LookupOAuthTokens("openai")
	if !ok {
		t.Fatal("expected LookupOAuthTokens to find saved credentials")
	}
	if cred.AccessToken != "mock-access-token-12345" {
		t.Fatalf("saved access token = %q, want 'mock-access-token-12345'", cred.AccessToken)
	}

	// Verify LookupAPIKey also returns the OAuth AccessToken
	apiKey, ok := LookupAPIKey("openai")
	if !ok || apiKey != "mock-access-token-12345" {
		t.Fatalf("LookupAPIKey = %q, %v; want 'mock-access-token-12345', true", apiKey, ok)
	}
}
