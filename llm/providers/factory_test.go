package providers

import (
	"context"
	"io"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

type authCaptureTransport struct {
	authorization string
}

type cancelAwareTransport struct {
	started chan struct{}
	once    sync.Once
}

func (t *cancelAwareTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	t.once.Do(func() { close(t.started) })
	<-req.Context().Done()
	return nil, req.Context().Err()
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
	provider, err := NewProviderFromConfig(context.Background(), &config.Config{
		Provider:               "openai",
		Model:                  "test-model",
		Endpoint:               "https://example.test/v1",
		APIKeyOverride:         "runtime-key",
		APIKeyOverrideProvider: "openai",
	}, llm.NewEndpointResolver(llm.EndpointResolverOptions{}))
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
			provider, err := NewProviderFromConfig(context.Background(), &config.Config{
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
			}, llm.NewEndpointResolver(llm.EndpointResolverOptions{}))
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
	_, err := NewProviderFromConfig(context.Background(), &config.Config{
		Provider: "openai",
		Models: []config.ModelDef{{
			Pattern:    "model",
			SystemRole: "assistant",
		}},
	}, llm.NewEndpointResolver(llm.EndpointResolverOptions{}))
	if err == nil || !strings.Contains(err.Error(), "invalid system role") {
		t.Fatalf("error = %v, want invalid system role", err)
	}
}

func TestNewProviderFromConfigPropagatesCancellationToEndpointProbe(t *testing.T) {
	transport := &cancelAwareTransport{started: make(chan struct{})}
	resolver := llm.NewEndpointResolver(llm.EndpointResolverOptions{
		HTTPClient: &http.Client{Transport: transport},
	})
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	done := make(chan error, 1)
	go func() {
		_, err := NewProviderFromConfig(ctx, &config.Config{
			Provider: "openai-compatible",
			Model:    "qwen",
		}, resolver)
		done <- err
	}()

	select {
	case <-transport.started:
	case <-time.After(time.Second):
		t.Fatal("provider construction did not start endpoint probing")
	}
	cancel()

	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("provider construction ignored canceled endpoint probe context")
	}
}
