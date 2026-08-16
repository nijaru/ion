package app

import (
	"testing"

	"github.com/nijaru/ion/config"
)

func TestHandleLoginCommandBrowserOAuth(t *testing.T) {
	tempDir := t.TempDir()
	t.Setenv("HOME", tempDir)

	m := readyModel(t)

	// 1. /login openai uses API-key setup; Codex owns browser OAuth.
	m, cmd := m.handleCommand("/login openai")
	if cmd != nil || m.Picker.Setup == nil || m.Picker.Setup.kind != SetupPromptAPIKey {
		t.Fatal("expected API-key setup for /login openai")
	}

	// 2. /login openai --key opens API key prompt
	m, _ = m.handleCommand("/login openai --key")
	if m.Picker.Setup == nil || m.Picker.Setup.kind != SetupPromptAPIKey {
		t.Fatalf("expected SetupPromptAPIKey for /login openai --key, got %#v", m.Picker.Setup)
	}

	// 3. /login openai-codex starts browser OAuth flow
	m.pickerReducer().closeSetup()
	m, cmd = m.handleCommand("/login openai-codex")
	if cmd == nil {
		t.Fatal("expected tea.Cmd for /login openai-codex")
	}

	// 4. handleOAuthLoginFinished updates credentials and outputs confirmation
	m, cmd = m.handleOAuthLoginFinished(oauthLoginFinishedMsg{
		provider: "openai-codex",
		tokens: &config.OAuthTokens{
			AccessToken: "test-token-123",
			ExpiresIn:   3600,
		},
	})
	if cmd == nil {
		t.Fatal("expected cmd on oauthLoginFinishedMsg")
	}
}
