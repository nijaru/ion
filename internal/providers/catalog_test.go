package providers

import (
	"context"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/config"
)

func TestProbeLocalAPIUsesConfiguredEndpoint(t *testing.T) {
	localProbeMu.Lock()
	localProbeCache = map[string]localProbeResult{}
	localProbeMu.Unlock()

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/models" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"data":[{"id":"local-model"}]}`))
	}))
	defer srv.Close()

	cfg := &config.Config{
		Provider: "local-api",
		Endpoint: srv.URL + "/v1",
	}

	endpoint, ok := ProbeLocalAPI(context.Background(), cfg)
	if !ok {
		t.Fatal("expected local api probe to succeed")
	}
	if endpoint != srv.URL+"/v1" {
		t.Fatalf("probe endpoint = %q, want %q", endpoint, srv.URL+"/v1")
	}
}

func TestCredentialStateContextReportsLocalAPIReadiness(t *testing.T) {
	localProbeMu.Lock()
	localProbeCache = map[string]localProbeResult{}
	localProbeMu.Unlock()

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"data":[{"id":"local-model"}]}`))
	}))
	defer srv.Close()

	def := MustLookup("local-api")
	cfg := &config.Config{
		Provider: "local-api",
		Endpoint: srv.URL + "/v1",
	}

	detail, ready := CredentialStateContext(context.Background(), cfg, def)
	if !ready {
		t.Fatal("expected local api to be ready")
	}
	if !strings.Contains(detail, "Ready at ") {
		t.Fatalf("detail = %q, want Ready at ...", detail)
	}
}

func TestCredentialStateContextReportsLocalAPINotRunning(t *testing.T) {
	localProbeMu.Lock()
	localProbeCache = map[string]localProbeResult{}
	localProbeMu.Unlock()

	def := MustLookup("local-api")
	cfg := &config.Config{
		Provider: "local-api",
		Endpoint: "http://127.0.0.1:1/v1",
	}

	detail, ready := CredentialStateContext(context.Background(), cfg, def)
	if ready {
		t.Fatal("expected local api to be unavailable")
	}
	if detail != "Not running" {
		t.Fatalf("detail = %q, want %q", detail, "Not running")
	}
}

func TestResolvedEndpointDoesNotLeakCustomEndpointToDefaultProviders(t *testing.T) {
	cfg := &config.Config{
		Provider: "openrouter",
		Endpoint: "http://fedora:8080/v1",
	}
	if got := ResolvedEndpoint(cfg); got != "https://openrouter.ai/api/v1" {
		t.Fatalf("resolved endpoint = %q, want OpenRouter default", got)
	}

	cfg.Provider = "local-api"
	if got := ResolvedEndpoint(cfg); got != "http://fedora:8080/v1" {
		t.Fatalf("local-api endpoint = %q, want configured endpoint", got)
	}
}

func TestCustomAuthAndHeadersDoNotLeakToDefaultProviders(t *testing.T) {
	cfg := &config.Config{
		Provider:     "openrouter",
		AuthEnvVar:   "LOCAL_API_KEY",
		ExtraHeaders: map[string]string{"X-Local": "1"},
	}
	if got := ResolvedAuthEnvVar(cfg); got != "OPENROUTER_API_KEY" {
		t.Fatalf("auth env = %q, want OpenRouter default", got)
	}
	if got := ResolvedHeaders(cfg); len(got) != 0 {
		t.Fatalf("headers = %#v, want none", got)
	}

	cfg.Provider = "local-api"
	if got := ResolvedAuthEnvVar(cfg); got != "LOCAL_API_KEY" {
		t.Fatalf("local auth env = %q, want configured override", got)
	}
	if got := ResolvedHeaders(cfg); got["X-Local"] != "1" {
		t.Fatalf("local headers = %#v, want configured header", got)
	}
}

func TestCredentialEnvVarsIncludesCatalogAndCustomAuth(t *testing.T) {
	got := CredentialEnvVars(&config.Config{AuthEnvVar: "LOCAL_API_KEY"})
	for _, want := range []string{
		"LOCAL_API_KEY",
		"OPENAI_API_KEY",
		"ANTHROPIC_API_KEY",
		"OPENROUTER_API_KEY",
		"GEMINI_API_KEY",
		"GOOGLE_API_KEY",
	} {
		if !slices.Contains(got, want) {
			t.Fatalf("credential env vars missing %q: %#v", want, got)
		}
	}
	if !slices.IsSorted(got) {
		t.Fatalf("credential env vars are not sorted: %#v", got)
	}
}

func TestResolvedEndpointIncludesZAIEndpoint(t *testing.T) {
	cfg := &config.Config{Provider: "zai"}
	if got := ResolvedEndpoint(cfg); got != "https://api.z.ai/api/paas/v4" {
		t.Fatalf("zai endpoint = %q, want Z.AI OpenAI-compatible endpoint", got)
	}
}

func TestShowInPickerDoesNotTreatEndpointAsCustomProviderSelection(t *testing.T) {
	custom := MustLookup("openai-compatible")
	cfg := &config.Config{
		Provider: "local-api",
		Endpoint: "http://fedora:8080/v1",
	}
	if ShowInPicker(cfg, custom) {
		t.Fatal("custom provider should stay hidden when endpoint belongs to local-api")
	}

	cfg.Provider = "openai-compatible"
	if !ShowInPicker(cfg, custom) {
		t.Fatal("custom provider should show when it is the active provider")
	}
}
