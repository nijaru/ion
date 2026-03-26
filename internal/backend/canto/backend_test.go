package canto

import (
	"context"
	"testing"
	"time"
)

func TestProviderAndModelFallBackToEnv(t *testing.T) {
	t.Setenv("ION_PROVIDER", "anthropic")
	t.Setenv("ION_MODEL", "claude-sonnet-4-5")

	b := New()

	if got := b.Provider(); got != "anthropic" {
		t.Fatalf("Provider() = %q, want %q", got, "anthropic")
	}
	if got := b.Model(); got != "claude-sonnet-4-5" {
		t.Fatalf("Model() = %q, want %q", got, "claude-sonnet-4-5")
	}
}

func TestResumeDoesNotDeadlockWhenBackendNeedsOpen(t *testing.T) {
	b := New()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- b.Resume(ctx, "session-id")
	}()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("expected resume to fail without provider/model")
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("resume appears to deadlock")
	}
}

func TestCloseIsIdempotent(t *testing.T) {
	b := New()

	if err := b.Close(); err != nil {
		t.Fatalf("first close: %v", err)
	}
	if err := b.Close(); err != nil {
		t.Fatalf("second close: %v", err)
	}
}
