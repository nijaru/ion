package gemini

import (
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

func TestNewProviderDefaults(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{})

	if got, want := p.ID(), "gemini"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://generativelanguage.googleapis.com/v1beta/openai/"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}

func TestNewProviderRespectsConfig(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		ID:          "gemini-custom",
		APIEndpoint: "https://example.test/gemini",
	})

	if got, want := p.ID(), "gemini-custom"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://example.test/gemini"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}
