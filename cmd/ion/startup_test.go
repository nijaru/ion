package main

import (
	"context"
	"errors"
	"testing"

	"github.com/nijaru/ion/config"
)

func TestResolveStartupConfigHonorsCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	cancel()

	err := resolveStartupConfig(ctx, &config.Config{
		Provider: "openai-compatible",
		Model:    "qwen",
	}, nil)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("resolveStartupConfig() error = %v, want context.Canceled", err)
	}
}
