package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func clearLiveProfileEnv(t *testing.T) {
	t.Helper()
	for _, name := range []string{
		"ION_LIVE_PROVIDER_A",
		"ION_LIVE_MODEL_A",
		"ION_LIVE_ENDPOINT_A",
		"ION_LIVE_THINKING_A",
		"ION_LIVE_PROVIDER_B",
		"ION_LIVE_MODEL_B",
		"ION_LIVE_ENDPOINT_B",
		"ION_LIVE_THINKING_B",
	} {
		t.Setenv(name, "")
	}
	t.Setenv("HOME", t.TempDir())
}

func TestLoadLiveProviderProfilesRequiresBothProfiles(t *testing.T) {
	clearLiveProfileEnv(t)
	t.Setenv("ION_LIVE_PROVIDER_A", "openai")
	t.Setenv("ION_LIVE_MODEL_A", "gpt-4.1")

	_, err := loadLiveProviderProfiles()
	if err == nil || !strings.Contains(err.Error(), "provider B") {
		t.Fatalf("error = %v, want missing provider B failure", err)
	}
}

func TestLoadLiveProviderProfilesRejectsSameAdapterAliases(t *testing.T) {
	clearLiveProfileEnv(t)
	t.Setenv("ION_LIVE_PROVIDER_A", "zai")
	t.Setenv("ION_LIVE_MODEL_A", "provider-a/model")
	t.Setenv("ION_LIVE_PROVIDER_B", "z-ai")
	t.Setenv("ION_LIVE_MODEL_B", "provider-b/model")

	_, err := loadLiveProviderProfiles()
	if err == nil || !strings.Contains(err.Error(), "materially different") {
		t.Fatalf("error = %v, want same-adapter rejection", err)
	}
}

func TestLoadLiveProviderProfilesCanonicalizesThinkingAndEndpoints(t *testing.T) {
	clearLiveProfileEnv(t)
	t.Setenv("ION_LIVE_PROVIDER_A", "openai")
	t.Setenv("ION_LIVE_MODEL_A", "gpt-4.1")
	t.Setenv("ION_LIVE_ENDPOINT_A", " https://provider-a.example/v1 ")
	t.Setenv("ION_LIVE_THINKING_A", " HIGH ")
	t.Setenv("ION_LIVE_PROVIDER_B", "gemini")
	t.Setenv("ION_LIVE_MODEL_B", "gemini-2.5-flash")

	profiles, err := loadLiveProviderProfiles()
	if err != nil {
		t.Fatalf("load profiles: %v", err)
	}
	if len(profiles) != 2 {
		t.Fatalf("profiles = %#v, want two profiles", profiles)
	}
	if profiles[0].provider != "openai" || profiles[1].provider != "gemini" {
		t.Fatalf("providers = %q, %q; want canonical ids", profiles[0].provider, profiles[1].provider)
	}
	if profiles[0].endpoint != "https://provider-a.example/v1" || profiles[0].thinking != "high" {
		t.Fatalf("profile A = %#v, want trimmed endpoint and thinking", profiles[0])
	}
	if profiles[1].thinking != "auto" {
		t.Fatalf("profile B thinking = %q, want auto", profiles[1].thinking)
	}
}

func TestLoadLiveProviderProfilesUsesStableConfigOnlyForProfileA(t *testing.T) {
	clearLiveProfileEnv(t)
	configDir := filepath.Join(os.Getenv("HOME"), ".ion")
	if err := os.MkdirAll(configDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(configDir, "config.toml"), []byte("provider = \"anthropic\"\nmodel = \"claude-sonnet-4\"\nendpoint = \"https://stable.example/v1\"\nauth_env_var = \"STABLE_LIVE_KEY\"\n[extra_headers]\nX-Stable = \"preserved\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("ION_LIVE_PROVIDER_B", "gemini")
	t.Setenv("ION_LIVE_MODEL_B", "gemini-2.5-flash")

	profiles, err := loadLiveProviderProfiles()
	if err != nil {
		t.Fatalf("load profiles from stable config: %v", err)
	}
	if profiles[0].provider != "anthropic" || profiles[0].model != "claude-sonnet-4" {
		t.Fatalf("profile A = %#v, want stable config values", profiles[0])
	}
	if profiles[0].providerConfig.Endpoint != "https://stable.example/v1" ||
		profiles[0].providerConfig.AuthEnvVar != "STABLE_LIVE_KEY" ||
		profiles[0].providerConfig.ExtraHeaders["X-Stable"] != "preserved" {
		t.Fatalf("profile A provider config = %#v, want stable endpoint/auth/header", profiles[0].providerConfig)
	}
	if profiles[1].provider != "gemini" || profiles[1].model != "gemini-2.5-flash" {
		t.Fatalf("profile B = %#v, want explicit environment values", profiles[1])
	}
}
