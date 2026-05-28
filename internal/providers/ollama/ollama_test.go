package ollama

import (
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

func TestNewDefaults(t *testing.T) {
	p := New()

	if got, want := p.ID(), "ollama"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "http://localhost:11434/v1"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}

func TestNewProviderRespectsConfig(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		ID:          "ollama-custom",
		APIEndpoint: "http://example.test/v1",
	})

	if got, want := p.ID(), "ollama-custom"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "http://example.test/v1"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}
