package agent

import (
	"context"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type harnessRetryProvider struct {
	streamCalls atomic.Int32
}

func (p *harnessRetryProvider) ID() string { return "harness-retry-test" }
func (p *harnessRetryProvider) Generate(context.Context, *llm.Request) (*llm.Response, error) {
	return &llm.Response{}, nil
}

func (p *harnessRetryProvider) Stream(context.Context, *llm.Request) (llm.Stream, error) {
	if p.streamCalls.Add(1) == 1 {
		return nil, syscall.ECONNRESET
	}
	return &mockStream{chunks: []*llm.Chunk{{Content: "recovered", StopReason: "stop"}}}, nil
}
func (p *harnessRetryProvider) Models(context.Context) ([]llm.Model, error) { return nil, nil }
func (p *harnessRetryProvider) CountTokens(context.Context, string, []llm.Message) (int, error) {
	return 0, nil
}
func (p *harnessRetryProvider) Cost(context.Context, string, llm.Usage) float64 { return 0 }
func (p *harnessRetryProvider) Capabilities(string) llm.Capabilities            { return llm.Capabilities{} }

func (p *harnessRetryProvider) IsTransient(err error) bool {
	return llm.IsTransientTransportError(err)
}
func (p *harnessRetryProvider) IsContextOverflow(error) bool { return false }

func TestHarnessEmitsRuntimeProviderRetryEvent(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	base := &harnessRetryProvider{}
	retry := llm.NewRetryProvider(base)
	retry.Config = llm.RetryConfig{
		MaxAttempts: 2,
		MinInterval: time.Nanosecond,
		MaxInterval: time.Nanosecond,
		Multiplier:  1,
	}
	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: retry.Stream,
	})
	defer h.Close()

	events, err := collectWithSubscribe(t, h, "retry me")
	if err != nil {
		t.Fatalf("Prompt() error = %v", err)
	}
	if got := base.streamCalls.Load(); got != 2 {
		t.Fatalf("provider attempts = %d, want 2", got)
	}
	var retries []session.ProviderRetry
	for _, event := range events {
		if retryEvent, ok := event.(session.ProviderRetry); ok {
			retries = append(retries, retryEvent)
		}
	}
	if len(retries) != 1 || retries[0].Attempt != 1 {
		t.Fatalf("provider retry events = %#v, want one attempt-1 event", retries)
	}
}
