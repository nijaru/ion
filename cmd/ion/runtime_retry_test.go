package main

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

type runtimeRetryProviderStub struct{}

func (runtimeRetryProviderStub) ID() string { return "runtime-retry-test" }
func (runtimeRetryProviderStub) Generate(context.Context, *llm.Request) (*llm.Response, error) {
	return &llm.Response{}, nil
}
func (runtimeRetryProviderStub) Stream(context.Context, *llm.Request) (llm.Stream, error) {
	return nil, nil
}
func (runtimeRetryProviderStub) Models(context.Context) ([]llm.Model, error) { return nil, nil }
func (runtimeRetryProviderStub) CountTokens(context.Context, string, []llm.Message) (int, error) {
	return 0, nil
}
func (runtimeRetryProviderStub) Cost(context.Context, string, llm.Usage) float64 { return 0 }
func (runtimeRetryProviderStub) Capabilities(string) llm.Capabilities            { return llm.Capabilities{} }
func (runtimeRetryProviderStub) IsTransient(error) bool                          { return true }
func (runtimeRetryProviderStub) IsContextOverflow(error) bool                    { return false }

func TestProviderWithRetryPolicyDefaultsToTransportUntilCancel(t *testing.T) {
	wrapped := providerWithRetryPolicy(runtimeRetryProviderStub{}, &config.Config{})
	retry, ok := wrapped.(*llm.RetryProvider)
	if !ok {
		t.Fatalf("provider type = %T, want *llm.RetryProvider", wrapped)
	}
	if !retry.Config.RetryForever || !retry.Config.RetryForeverTransportOnly {
		t.Fatalf("retry config = %#v, want transport retry until cancellation", retry.Config)
	}
	if retry.Config.MaxAttempts != 3 {
		t.Fatalf("max attempts = %d, want bounded provider retry default", retry.Config.MaxAttempts)
	}
	if retry.Config.MinInterval != time.Second {
		t.Fatalf("retry delay = %v, want 1s", retry.Config.MinInterval)
	}
}

func TestProviderWithRetryPolicyDisabledMakesOneAttempt(t *testing.T) {
	disabled := false
	wrapped := providerWithRetryPolicy(runtimeRetryProviderStub{}, &config.Config{
		RetryUntilCancelled: &disabled,
	})
	retry := wrapped.(*llm.RetryProvider)
	if retry.Config.RetryForever || retry.Config.RetryForeverTransportOnly != true {
		t.Fatalf("retry config = %#v, want disabled forever policy with transport classification", retry.Config)
	}
	if retry.Config.MaxAttempts != 1 {
		t.Fatalf("max attempts = %d, want 1", retry.Config.MaxAttempts)
	}
}

func TestProviderWithRetryPolicyNilProvider(t *testing.T) {
	if got := providerWithRetryPolicy(nil, nil); got != nil {
		t.Fatalf("nil provider = %T, want nil", got)
	}
}

func TestProviderWithRetryPolicyUsesConfiguredBounds(t *testing.T) {
	wrapped := providerWithRetryPolicy(runtimeRetryProviderStub{}, &config.Config{
		MaxRetries:       5,
		RetryBaseDelayMs: 250,
	})
	retry := wrapped.(*llm.RetryProvider)
	if retry.Config.MaxAttempts != 5 {
		t.Fatalf("max attempts = %d, want 5", retry.Config.MaxAttempts)
	}
	if retry.Config.MinInterval != 250*time.Millisecond {
		t.Fatalf("retry delay = %v, want 250ms", retry.Config.MinInterval)
	}
}
