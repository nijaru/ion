package providers

import (
	"context"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

type authCaptureTransport struct {
	authorization string
}

func (t *authCaptureTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	t.authorization = req.Header.Get("Authorization")
	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     make(http.Header),
		Body:       io.NopCloser(strings.NewReader("")),
		Request:    req,
	}, nil
}

func TestNewProviderFromConfigUsesRuntimeAPIKeyOverride(t *testing.T) {
	provider, err := NewProviderFromConfig(&config.Config{
		Provider:               "openai",
		Model:                  "test-model",
		Endpoint:               "https://example.test/v1",
		APIKeyOverride:         "runtime-key",
		APIKeyOverrideProvider: "openai",
	})
	if err != nil {
		t.Fatal(err)
	}
	transport := &authCaptureTransport{}
	stream, err := provider.Stream(context.Background(), &llm.Request{
		Model:     "test-model",
		Transport: transport,
	})
	if err != nil {
		t.Fatal(err)
	}
	_ = stream.Close()
	if transport.authorization != "Bearer runtime-key" {
		t.Fatalf("authorization = %q, want Bearer runtime-key", transport.authorization)
	}
}

func TestNewProviderFromConfigWiresModelCapabilities(t *testing.T) {
	tests := []struct {
		name          string
		provider      string
		model         string
		preset        string
		reasoningKind string
		systemRole    string
		wantKind      llm.ReasoningKind
		wantRole      llm.Role
	}{
		{
			name:       "openai developer reasoning",
			provider:   "openai",
			model:      "o4-mini",
			preset:     "openai-reasoning",
			systemRole: "developer",
			wantKind:   llm.ReasoningKindEffort,
			wantRole:   llm.RoleDeveloper,
		},
		{
			name:          "anthropic budget reasoning",
			provider:      "anthropic",
			model:         "claude-sonnet-4-20250514",
			preset:        "reasoning",
			reasoningKind: "budget",
			wantKind:      llm.ReasoningKindBudget,
			wantRole:      llm.RoleSystem,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			provider, err := NewProviderFromConfig(&config.Config{
				Provider:               tt.provider,
				Model:                  tt.model,
				APIKeyOverride:         "test-key",
				APIKeyOverrideProvider: tt.provider,
				Models: []config.ModelDef{{
					Pattern:       tt.model,
					Preset:        tt.preset,
					ReasoningKind: tt.reasoningKind,
					SystemRole:    tt.systemRole,
				}},
			})
			if err != nil {
				t.Fatal(err)
			}

			caps := provider.Capabilities(tt.model)
			if caps.Reasoning.Kind != tt.wantKind {
				t.Fatalf("reasoning kind = %q, want %q", caps.Reasoning.Kind, tt.wantKind)
			}
			if caps.SystemRole != tt.wantRole {
				t.Fatalf("system role = %q, want %q", caps.SystemRole, tt.wantRole)
			}
		})
	}
}

func TestNewProviderFromConfigRejectsInvalidModelCapability(t *testing.T) {
	_, err := NewProviderFromConfig(&config.Config{
		Provider: "openai",
		Models: []config.ModelDef{{
			Pattern:    "model",
			SystemRole: "assistant",
		}},
	})
	if err == nil || !strings.Contains(err.Error(), "invalid system role") {
		t.Fatalf("error = %v, want invalid system role", err)
	}
}
