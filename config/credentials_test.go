package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestSaveAPIKeyWritesPrivateCredentialFile(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveAPIKey("OpenAI", " sk-test "); err != nil {
		t.Fatalf("save api key: %v", err)
	}

	got, ok := LookupAPIKey("openai")
	if !ok || got != "sk-test" {
		t.Fatalf("credential = (%q, %v), want sk-test true", got, ok)
	}

	path := filepath.Join(home, ".ion", "credentials.toml")
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat credentials: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0o600 {
		t.Fatalf("credentials perm = %o, want 0600", perm)
	}
}

func TestAPIKeyProviderAliasesCanonicalize(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveAPIKey("local-api", "local-key"); err != nil {
		t.Fatalf("save api key: %v", err)
	}
	got, ok := LookupAPIKey("openai-compatible")
	if !ok || got != "local-key" {
		t.Fatalf("credential = (%q, %v), want local-key true", got, ok)
	}
	got, ok = LookupAPIKey("custom-api")
	if !ok || got != "local-key" {
		t.Fatalf("alias credential = (%q, %v), want local-key true", got, ok)
	}
}
