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
	errRetryTransient = errors.New("temporary upstream failure")
	errRetryPermanent = errors.New("invalid request")
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
	return errors.Is(err, errRetryTransient) || IsRateLimit(err) || IsTransientTransportError(err)
}
func (p *retryProviderStub) IsContextOverflow(error) bool { return false }

type retryStreamStub struct{}

func (*retryStreamStub) Next() (*Chunk, bool) { return nil, false }
func (*retryStreamStub) Err() error           { return nil }
func (*retryStreamStub) Close() error         { return nil }

type retryFaultStream struct {
	chunk      *Chunk
	ok         bool
	emitted    bool
	streamErr  error
	closeErr   error
	closeCalls atomic.Int32
}

func (s *retryFaultStream) Next() (*Chunk, bool) {
	if s.emitted {
		return nil, false
	}
	s.emitted = true
	return s.chunk, s.ok
}
func (s *retryFaultStream) Err() error { return s.streamErr }
func (s *retryFaultStream) Close() error {
	s.closeCalls.Add(1)
	return s.closeErr
}

type returnedStreamErrorProvider struct {
	retryProviderStub
	stream Stream
	calls  atomic.Int32
}

func (p *returnedStreamErrorProvider) Stream(context.Context, *Request) (Stream, error) {
	if p.calls.Add(1) == 1 {
		return p.stream, errRetryTransient
	}
	return &retryStreamStub{}, nil
}

type nilStreamResolverProvider struct{ retryProviderStub }

func (*nilStreamResolverProvider) Stream(context.Context, *Request) (Stream, error) {
	return nil, nil
}

type nilChunkProvider struct{ retryProviderStub }

func (*nilChunkProvider) Stream(context.Context, *Request) (Stream, error) {
	return &retryFaultStream{ok: true}, nil
}

type retryReadFailureProvider struct {
	calls              atomic.Int32
	afterChunk         bool
	firstCloseErr      error
	firstStream        Stream
	replacementErrors  []error
	replacementStreams []Stream
	retrySuccess       Stream
}

func (p *retryReadFailureProvider) ID() string { return "retry-read-test" }
func (p *retryReadFailureProvider) Generate(context.Context, *Request) (*Response, error) {
	return &Response{}, nil
}

func (p *retryReadFailureProvider) Stream(context.Context, *Request) (Stream, error) {
	call := int(p.calls.Add(1)) - 1
	if call == 0 {
		if p.firstStream != nil {
			return p.firstStream, nil
		}
		return &retryReadFailureStream{afterChunk: p.afterChunk, closeErr: p.firstCloseErr}, nil
	}
	replacement := call - 1
	if replacement < len(p.replacementErrors) {
		var replacementStream Stream
		if replacement < len(p.replacementStreams) {
			replacementStream = p.replacementStreams[replacement]
		}
		return replacementStream, p.replacementErrors[replacement]
	}
	if p.retrySuccess != nil {
		return p.retrySuccess, nil
	}
	return &retryStreamStub{}, nil
}
func (p *retryReadFailureProvider) Models(context.Context) ([]Model, error) { return nil, nil }
func (p *retryReadFailureProvider) CountTokens(context.Context, string, []Message) (int, error) {
	return 0, nil
}
func (p *retryReadFailureProvider) Cost(context.Context, string, Usage) float64 { return 0 }
func (p *retryReadFailureProvider) Capabilities(string) Capabilities            { return Capabilities{} }

func (p *retryReadFailureProvider) IsTransient(err error) bool {
	return errors.Is(err, errRetryTransient)
}
func (p *retryReadFailureProvider) IsContextOverflow(error) bool { return false }

type retryReadFailureStream struct {
	afterChunk   bool
	emitted      bool
	closeErr     error
	closeCounter *atomic.Int32
}

func (s *retryReadFailureStream) Next() (*Chunk, bool) {
	if !s.emitted && s.afterChunk {
		s.emitted = true
		return &Chunk{Content: "partial"}, true
	}
	s.emitted = true
	return nil, false
}

func (s *retryReadFailureStream) Err() error {
	if s.afterChunk && !s.emitted {
		return nil
	}
	return errRetryTransient
}

func (s *retryReadFailureStream) Close() error {
	if s.closeCounter != nil {
		s.closeCounter.Add(1)
	}
	return s.closeErr
}

type retryCompletionStream struct {
	chunk *Chunk
	done  bool
}

func (s *retryCompletionStream) Next() (*Chunk, bool) {
	if s.done {
		return nil, false
	}
	s.done = true
	return s.chunk, true
}
func (*retryCompletionStream) Err() error   { return nil }
func (*retryCompletionStream) Close() error { return nil }

func retryTestConfig() RetryConfig {
	return RetryConfig{
		MaxAttempts: 3,
		MinInterval: time.Nanosecond,
		MaxInterval: time.Nanosecond,
		Multiplier:  1,
	}
}

func TestRetryProviderRetriesSetupStreamErrorAfterSuccessfulClose(t *testing.T) {
	first := &retryFaultStream{streamErr: errRetryTransient}
	provider := &returnedStreamErrorProvider{stream: first}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("RetryProvider.Stream: %v", err)
	}
	if first.closeCalls.Load() != 1 {
		t.Fatalf("returned stream close calls = %d, want one", first.closeCalls.Load())
	}
	if provider.calls.Load() != 2 {
		t.Fatalf("provider calls = %d, want one retry", provider.calls.Load())
	}
	if err := stream.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestRetryProviderClosesStreamReturnedWithSetupError(t *testing.T) {
	first := &retryFaultStream{closeErr: nil}
	provider := &returnedStreamErrorProvider{stream: first}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("RetryProvider.Stream: %v", err)
	}
	if first.closeCalls.Load() != 1 {
		t.Fatalf("returned stream close calls = %d, want one", first.closeCalls.Load())
	}
	if err := stream.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestRetryProviderDoesNotRetryWhenReturnedStreamCleanupFails(t *testing.T) {
	closeErr := errors.New("setup stream cleanup failed")
	first := &retryFaultStream{closeErr: closeErr}
	provider := &returnedStreamErrorProvider{stream: first}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	_, err := retry.Stream(t.Context(), &Request{})
	if !errors.Is(err, closeErr) {
		t.Fatalf("RetryProvider.Stream error = %v, want cleanup failure", err)
	}
	if provider.calls.Load() != 1 {
		t.Fatalf("provider calls = %d, want no retry after cleanup failure", provider.calls.Load())
	}
}

func TestRetryProviderRetriesTransientStreamFailures(t *testing.T) {
	provider := &retryProviderStub{
		streamErrors: []error{errRetryTransient, errRetryTransient},
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

func TestRetryProviderRetriesTransientReadFailureBeforeOutput(t *testing.T) {
	provider := &retryReadFailureProvider{}
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
	if _, ok := stream.Next(); ok {
		t.Fatal("retried empty stream yielded a chunk")
	}
	if err := stream.Err(); err != nil {
		t.Fatalf("retried stream error = %v, want nil", err)
	}
	if got := provider.calls.Load(); got != 2 {
		t.Fatalf("provider attempts = %d, want 2", got)
	}
	if len(events) != 1 || events[0].Attempt != 1 {
		t.Fatalf("retry events = %#v, want one attempt-1 event", events)
	}
}

func TestRetryProviderDoesNotCloseFailedStreamAgainDuringReplacement(t *testing.T) {
	closeCount := atomic.Int32{}
	provider := &retryReadFailureProvider{
		firstStream:       &retryReadFailureStream{closeCounter: &closeCount},
		replacementErrors: []error{errRetryTransient, errRetryTransient},
		retrySuccess:      &retryCompletionStream{chunk: &Chunk{Content: "complete"}},
	}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()
	retry.Config.MaxAttempts = 4

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("Stream() error = %v", err)
	}
	chunk, ok := stream.Next()
	if !ok || chunk == nil || chunk.Content != "complete" {
		t.Fatalf("retried chunk = %#v, %t, want complete", chunk, ok)
	}
	if got := closeCount.Load(); got != 1 {
		t.Fatalf("failed stream close count = %d, want one", got)
	}
	if got := provider.calls.Load(); got != 4 {
		t.Fatalf("provider attempts = %d, want initial plus two failures and success", got)
	}
}

func TestRetryProviderStopsWhenReplacementStreamCannotClose(t *testing.T) {
	closeErr := errors.New("replacement stream close failed")
	provider := &retryReadFailureProvider{
		replacementErrors:  []error{errRetryTransient},
		replacementStreams: []Stream{&retryFaultStream{closeErr: closeErr}},
	}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("Stream() error = %v", err)
	}
	if _, ok := stream.Next(); ok {
		t.Fatal("replacement stream yielded a chunk")
	}
	if !errors.Is(stream.Err(), closeErr) {
		t.Fatalf("stream error = %v, want replacement close failure", stream.Err())
	}
	if got := provider.calls.Load(); got != 2 {
		t.Fatalf("provider attempts = %d, want initial plus one replacement", got)
	}
}

func TestRetryProviderStopsWhenFailedStreamCannotClose(t *testing.T) {
	closeErr := errors.New("failed stream close")
	provider := &retryReadFailureProvider{firstCloseErr: closeErr}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("Stream() error = %v", err)
	}
	if _, ok := stream.Next(); ok {
		t.Fatal("failed stream yielded a chunk")
	}
	if !errors.Is(stream.Err(), closeErr) {
		t.Fatalf("stream error = %v, want close failure", stream.Err())
	}
	if got := provider.calls.Load(); got != 1 {
		t.Fatalf("provider attempts = %d, want no retry after close failure", got)
	}
}

func TestRetryProviderMarksFinalCloseFailureAsTerminal(t *testing.T) {
	closeErr := errors.New("connection reset during stream close")
	provider := &retryReadFailureProvider{
		retrySuccess: &retryFaultStream{closeErr: closeErr},
	}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("Stream() error = %v", err)
	}
	if _, ok := stream.Next(); ok {
		t.Fatal("replacement stream yielded a chunk")
	}
	if err := stream.Close(); !errors.Is(err, closeErr) {
		t.Fatalf("stream close error = %v, want close failure", err)
	} else if !IsStreamCleanupError(err) {
		t.Fatalf("stream close error = %v, want cleanup marker", err)
	}
}

func TestRetryProviderDoesNotReplayAfterStreamOutput(t *testing.T) {
	provider := &retryReadFailureProvider{afterChunk: true}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("Stream() error = %v", err)
	}
	if chunk, ok := stream.Next(); !ok || chunk == nil || chunk.Content != "partial" {
		t.Fatalf("first chunk = %#v, %t, want partial output", chunk, ok)
	}
	if _, ok := stream.Next(); ok {
		t.Fatal("failed stream yielded an unexpected second chunk")
	}
	if !errors.Is(stream.Err(), errRetryTransient) {
		t.Fatalf("stream error = %v, want transient failure", stream.Err())
	}
	if got := provider.calls.Load(); got != 1 {
		t.Fatalf("provider attempts = %d, want no replay after output", got)
	}
}

func TestCollectStreamWithRetryDiscardsPartialAttempt(t *testing.T) {
	provider := &retryReadFailureProvider{
		afterChunk: true,
		retrySuccess: &retryCompletionStream{
			chunk: &Chunk{Content: "complete"},
		},
	}
	chunks, err := CollectStreamWithRetry(
		t.Context(),
		&Request{},
		provider.Stream,
		StreamRetryPolicy{Config: retryTestConfig(), IsTransient: provider.IsTransient},
	)
	if err != nil {
		t.Fatalf("CollectStreamWithRetry() error = %v", err)
	}
	if len(chunks) != 1 || chunks[0] == nil || chunks[0].Content != "complete" {
		t.Fatalf("collected chunks = %#v, want only complete retry", chunks)
	}
	if got := provider.calls.Load(); got != 2 {
		t.Fatalf("provider attempts = %d, want one retry after partial failure", got)
	}
}

func TestCollectStreamWithRetryReportsCloseFailure(t *testing.T) {
	closeErr := errors.New("summary stream close failed")
	chunks, err := CollectStreamWithRetry(
		t.Context(),
		&Request{},
		func(context.Context, *Request) (Stream, error) {
			return &retryFaultStream{
				chunk:    &Chunk{Content: "summary"},
				ok:       true,
				closeErr: closeErr,
			}, nil
		},
		StreamRetryPolicy{},
	)
	if !errors.Is(err, closeErr) {
		t.Fatalf("CollectStreamWithRetry() error = %v, want close failure", err)
	}
	if chunks != nil {
		t.Fatalf("collected chunks = %#v, want nil on close failure", chunks)
	}
}

func TestRetryProviderRejectsNilStreamChunk(t *testing.T) {
	retry := NewRetryProvider(&nilChunkProvider{})
	retry.Config = retryTestConfig()

	stream, err := retry.Stream(t.Context(), &Request{})
	if err != nil {
		t.Fatalf("Stream() error = %v", err)
	}
	if chunk, ok := stream.Next(); ok || chunk != nil {
		t.Fatalf("first chunk = %#v, %t, want terminal nil chunk", chunk, ok)
	}
	if !errors.Is(stream.Err(), errNilStreamChunk) {
		t.Fatalf("stream error = %v, want nil-chunk protocol error", stream.Err())
	}
}

func TestSmartResolverRejectsNilStream(t *testing.T) {
	resolver := NewSmartResolver(StrategyPriority, &nilStreamResolverProvider{})
	stream, err := resolver.Stream(t.Context(), &Request{})
	if stream != nil {
		t.Fatalf("Stream() stream = %#v, want nil", stream)
	}
	if !errors.Is(err, errProviderNilStream) {
		t.Fatalf("Stream() error = %v, want nil-provider-stream error", err)
	}
}

func TestRetryProviderDisabledMakesOneAttempt(t *testing.T) {
	provider := &retryProviderStub{streamErrors: []error{errRetryTransient}}
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
		streamErrors: []error{errRetryTransient, errRetryTransient, errRetryTransient},
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
		generateErrors: []error{errRetryTransient, errRetryTransient},
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
	provider := &retryProviderStub{streamErrors: []error{errRetryPermanent}}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()

	_, err := retry.Stream(t.Context(), &Request{})
	if !errors.Is(err, errRetryPermanent) {
		t.Fatalf("Stream() error = %v, want permanent error", err)
	}
	if got := provider.streamCalls.Load(); got != 1 {
		t.Fatalf("stream attempts = %d, want 1", got)
	}
}

func TestRetryProviderUsesBoundedServerRetryAfter(t *testing.T) {
	provider := &retryProviderStub{
		streamErrors: []error{NewHTTPErrorWithRetryAfter("test", 429, nil, 2*time.Second)},
	}
	retry := NewRetryProvider(provider)
	retry.Config = retryTestConfig()
	retry.Config.MinInterval = time.Millisecond
	retry.Config.MaxInterval = 100 * time.Millisecond

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	var event RetryEvent
	streamCtx := WithRetryObserver(ctx, func(got RetryEvent) {
		event = got
		cancel()
	})
	_, err := retry.Stream(streamCtx, &Request{})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Stream() error = %v, want context.Canceled", err)
	}
	if event.Delay != retry.Config.MaxInterval {
		t.Fatalf("retry delay = %s, want configured max %s", event.Delay, retry.Config.MaxInterval)
	}
}
