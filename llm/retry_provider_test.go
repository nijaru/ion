package llm

import (
	"context"
	"errors"
	"sync/atomic"
	"syscall"
	"testing"
	"time"
)

var (
	retryTransientErr = errors.New("temporary upstream failure")
	retryPermanentErr = errors.New("invalid request")
)

type retryProviderStub struct {
	streamCalls    atomic.Int32
	streamErrors   []error
	generateCalls  atomic.Int32
	generateErrors []error
}

func (p *retryProviderStub) ID() string { return "retry-test" }

func (p *retryProviderStub) Generate(context.Context, *Request) (*Response, error) {
	call := int(p.generateCalls.Add(1)) - 1
	if call < len(p.generateErrors) {
		return nil, p.generateErrors[call]
	}
	return &Response{}, nil
}

func (p *retryProviderStub) Stream(context.Context, *Request) (Stream, error) {
	call := int(p.streamCalls.Add(1)) - 1
	if call < len(p.streamErrors) {
		return nil, p.streamErrors[call]
	}
	return &retryStreamStub{}, nil
}

func (p *retryProviderStub) Models(context.Context) ([]Model, error) { return nil, nil }
func (p *retryProviderStub) CountTokens(context.Context, string, []Message) (int, error) {
	return 0, nil
}
func (p *retryProviderStub) Cost(context.Context, string, Usage) float64 { return 0 }
func (p *retryProviderStub) Capabilities(string) Capabilities            { return Capabilities{} }
func (p *retryProviderStub) IsTransient(err error) bool {
	return errors.Is(err, retryTransientErr) || IsTransientTransportError(err)
}
func (p *retryProviderStub) IsContextOverflow(error) bool { return false }

type retryStreamStub struct{}

func (*retryStreamStub) Next() (*Chunk, bool) { return nil, false }
func (*retryStreamStub) Err() error           { return nil }
func (*retryStreamStub) Close() error         { return nil }

func retryTestConfig() RetryConfig {
	return RetryConfig{
		MaxAttempts: 3,
		MinInterval: time.Nanosecond,
		MaxInterval: time.Nanosecond,
		Multiplier:  1,
	}
}

func TestRetryProviderRetriesTransientStreamFailures(t *testing.T) {
	provider := &retryProviderStub{
		streamErrors: []error{retryTransientErr, retryTransientErr},
	}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()
	var events []RetryEvent
	ctx := WithRetryObserver(t.Context(), func(event RetryEvent) {
		events = append(events, event)
	})

	stream, err := retry.Stream(ctx, &Request{})
	if err != nil {
		t.Fatalf("Stream() error = %v", err)
	}
	if stream == nil {
		t.Fatal("Stream() returned nil stream")
	}
	if got := provider.streamCalls.Load(); got != 3 {
		t.Fatalf("stream attempts = %d, want 3", got)
	}
	if len(events) != 2 || events[0].Attempt != 1 || events[1].Attempt != 2 {
		t.Fatalf("retry events = %#v, want attempts 1 and 2", events)
	}
}

func TestRetryProviderDisabledMakesOneAttempt(t *testing.T) {
	provider := &retryProviderStub{streamErrors: []error{retryTransientErr}}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()
	retry.Config.MaxAttempts = 1

	_, err := retry.Stream(t.Context(), &Request{})
	if err == nil {
		t.Fatal("Stream() error = nil, want exhausted retry error")
	}
	var exhausted *RetryExhaustedError
	if !errors.As(err, &exhausted) || exhausted.Attempts != 1 {
		t.Fatalf("error = %v, want one-attempt RetryExhaustedError", err)
	}
	if got := provider.streamCalls.Load(); got != 1 {
		t.Fatalf("stream attempts = %d, want 1", got)
	}
}

func TestRetryProviderTransportRetryStopsOnCancellation(t *testing.T) {
	provider := &retryProviderStub{streamErrors: []error{syscall.ECONNRESET}}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()
	retry.Config.RetryForever = true
	retry.Config.RetryForeverTransportOnly = true
	retry.Config.MinInterval = time.Second
	retry.Config.MaxInterval = time.Second

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	streamCtx := WithRetryObserver(ctx, func(RetryEvent) { cancel() })
	_, err := retry.Stream(streamCtx, &Request{})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Stream() error = %v, want context.Canceled", err)
	}
	if got := provider.streamCalls.Load(); got != 1 {
		t.Fatalf("stream attempts = %d, want one canceled backoff", got)
	}
}

func TestRetryProviderTransportOnlyForeverBoundsProviderFailures(t *testing.T) {
	provider := &retryProviderStub{
		streamErrors: []error{retryTransientErr, retryTransientErr, retryTransientErr},
	}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()
	retry.Config.RetryForever = true
	retry.Config.RetryForeverTransportOnly = true

	_, err := retry.Stream(t.Context(), &Request{})
	if err == nil {
		t.Fatal("Stream() error = nil, want bounded retry error")
	}
	var exhausted *RetryExhaustedError
	if !errors.As(err, &exhausted) || exhausted.Attempts != 3 {
		t.Fatalf("error = %v, want three-attempt RetryExhaustedError", err)
	}
	if got := provider.streamCalls.Load(); got != 3 {
		t.Fatalf("stream attempts = %d, want 3", got)
	}
}

func TestRetryProviderRetriesGenerateFailures(t *testing.T) {
	provider := &retryProviderStub{
		generateErrors: []error{retryTransientErr, retryTransientErr},
	}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	if _, err := retry.Generate(t.Context(), &Request{}); err != nil {
		t.Fatalf("Generate() error = %v", err)
	}
	if got := provider.generateCalls.Load(); got != 3 {
		t.Fatalf("generate attempts = %d, want 3", got)
	}
}

func TestRetryProviderDoesNotRetryPermanentFailure(t *testing.T) {
	provider := &retryProviderStub{streamErrors: []error{retryPermanentErr}}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	_, err := retry.Stream(t.Context(), &Request{})
	if !errors.Is(err, retryPermanentErr) {
		t.Fatalf("Stream() error = %v, want permanent error", err)
	}
	if got := provider.streamCalls.Load(); got != 1 {
		t.Fatalf("stream attempts = %d, want 1", got)
	}
}
