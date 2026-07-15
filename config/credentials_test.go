package config

import (
	"os"
	"path/filepath"
	"strings"
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

func TestSaveAPIKeyReplacesCredentialsAtomically(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveAPIKey("openai", "first-key"); err != nil {
		t.Fatalf("save first api key: %v", err)
	}
	if err := SaveAPIKey("openai", "second-key"); err != nil {
		t.Fatalf("save replacement api key: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(home, ".ion", "credentials.toml"))
	if err != nil {
		t.Fatalf("read credentials: %v", err)
	}
	got := string(data)
	if !strings.Contains(got, "second-key") {
		t.Fatalf("credentials missing replacement key:\n%s", got)
	}
	if strings.Contains(got, "first-key") {
		t.Fatalf("credentials retained stale key:\n%s", got)
	}
	matches, err := filepath.Glob(filepath.Join(home, ".ion", ".credentials.toml.tmp-*"))
	if err != nil {
		t.Fatalf("glob temporary credentials: %v", err)
	}
	if len(matches) != 0 {
		t.Fatalf("temporary credentials files left behind: %v", matches)
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
