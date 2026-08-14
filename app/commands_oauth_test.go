package app

import (
	"testing"

	"github.com/nijaru/ion/config"
)

func TestHandleLoginCommandBrowserOAuth(t *testing.T) {
	tempDir := t.TempDir()
	t.Setenv("HOME", tempDir)

	m := readyModel(t)

	// 1. /login openai starts browser OAuth flow
	m, cmd := m.handleCommand("/login openai")
	if cmd == nil {
		t.Fatal("expected tea.Cmd for /login openai")
	}

	// 2. /login openai --key opens API key prompt
	m, _ = m.handleCommand("/login openai --key")
	if m.Picker.Setup == nil || m.Picker.Setup.kind != SetupPromptAPIKey {
		t.Fatalf("expected SetupPromptAPIKey for /login openai --key, got %#v", m.Picker.Setup)
	}

	// 3. /login chatgpt starts browser OAuth flow
	m.pickerReducer().closeSetup()
	m, cmd = m.handleCommand("/login chatgpt")
	if cmd == nil {
		t.Fatal("expected tea.Cmd for /login chatgpt")
	}

	// 4. handleOAuthLoginFinished updates credentials and outputs confirmation
	m, cmd = m.handleOAuthLoginFinished(oauthLoginFinishedMsg{
		provider: "openai",
		tokens: &config.OAuthTokens{
			AccessToken: "test-token-123",
			ExpiresIn:   3600,
		},
	})
	if cmd == nil {
		t.Fatal("expected cmd on oauthLoginFinishedMsg")
	}
}
