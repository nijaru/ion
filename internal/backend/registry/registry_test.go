package registry

import (
	"context"
	"sync"
	"testing"
)

func TestGetMetadataUsesInjectedFetcher(t *testing.T) {
	registryOnce = sync.Once{}
	registryCache = nil

	oldFetcher := metadataFetcher
	defer func() { metadataFetcher = oldFetcher }()

	metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		return ModelMetadata{
			ID:           model,
			Provider:     provider,
			ContextLimit: 123000,
			InputPrice:   0.1,
			OutputPrice:  0.2,
			UpdatedAt:    1,
		}, nil
	}

	meta, ok := GetMetadata(context.Background(), "openrouter", "openai/gpt-5.4")
	if !ok {
		t.Fatal("expected metadata fetch to succeed")
	}
	if meta.Provider != "openrouter" || meta.ID != "openai/gpt-5.4" {
		t.Fatalf("unexpected metadata: %#v", meta)
	}
}
