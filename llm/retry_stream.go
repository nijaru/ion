package llm

import (
	"context"
	"errors"
	"time"
)

// StreamRetryPolicy controls replay-safe stream collection. It is intended for
// internal requests whose partial output has not crossed a user-visible or
// durable boundary, such as context summaries. Normal assistant streams use
// RetryProvider.Stream and never replay after output.
type StreamRetryPolicy struct {
	Config      RetryConfig
	IsTransient func(error) bool
}

// CollectStreamWithRetry consumes a replay-safe stream to completion. A failed
// attempt's chunks are discarded before a transient retry, so a partial
// summary cannot be duplicated in the next attempt. Callers must not expose
// collected chunks until this function succeeds.
func CollectStreamWithRetry(
	ctx context.Context,
	req *Request,
	streamFn func(context.Context, *Request) (Stream, error),
	policy StreamRetryPolicy,
) ([]*Chunk, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	if streamFn == nil {
		return nil, errors.New("llm: replay-safe stream function is not configured")
	}

	cfg := normalizedRetryConfig(policy.Config)
	interval := cfg.MinInterval
	for attempts := 1; ; attempts++ {
		chunks, err := collectStreamAttempt(ctx, req, streamFn)
		if err == nil {
			return chunks, nil
		}
		if ctxErr := ctx.Err(); ctxErr != nil {
			return nil, ctxErr
		}
		if policy.IsTransient == nil || !policy.IsTransient(err) {
			return nil, err
		}
		if retryLimitReached(cfg, attempts, err) {
			return nil, retryExhausted(attempts, err)
		}

		delay := retryDelay(cfg, interval, err)
		notifyRetry(ctx, cfg, RetryEvent{Attempt: attempts, Delay: delay, Err: err})
		if err := waitForRetry(ctx, delay); err != nil {
			return nil, err
		}
		interval = advanceRetryInterval(cfg, interval)
	}
}

func collectStreamAttempt(
	ctx context.Context,
	req *Request,
	streamFn func(context.Context, *Request) (Stream, error),
) (chunks []*Chunk, err error) {
	stream, err := streamFn(ctx, req)
	if err != nil {
		return nil, err
	}
	if stream == nil {
		return nil, errProviderNilStream
	}

	defer func() {
		err = errors.Join(err, stream.Close())
	}()
	for {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		default:
		}
		chunk, ok := stream.Next()
		if ok {
			if chunk == nil {
				return nil, errNilStreamChunk
			}
			chunks = append(chunks, chunk)
			continue
		}
		streamErr := stream.Err()
		if streamErr != nil {
			return nil, streamErr
		}
		return chunks, nil
	}
}

// retryStream retries a provider stream only when it fails before yielding its
// first chunk. Once a chunk has crossed the stream boundary, replaying the
// request could duplicate visible output or an eventual tool call, so the
// error remains terminal and the controller owns the normal failure path.
type retryStream struct {
	ctx      context.Context
	request  *Request
	provider Provider
	config   RetryConfig
	stream   Stream
	attempts int
	interval time.Duration
	emitted  bool
	closed   bool
	err      error
}

func (s *retryStream) Next() (*Chunk, bool) {
	if s == nil || s.closed || s.stream == nil {
		return nil, false
	}
	for {
		chunk, ok := s.stream.Next()
		if ok {
			if chunk == nil {
				s.err = errNilStreamChunk
				return nil, false
			}
			s.emitted = true
			return chunk, true
		}
		streamErr := s.stream.Err()
		if streamErr == nil {
			return nil, false
		}
		if s.emitted || !s.provider.IsTransient(streamErr) {
			s.err = streamErr
			return nil, false
		}
		if retryErr := s.retryAfter(streamErr); retryErr != nil {
			s.err = retryErr
			return nil, false
		}
	}
}

func (s *retryStream) retryAfter(streamErr error) error {
	for {
		if retryLimitReached(s.config, s.attempts, streamErr) {
			return retryExhausted(s.attempts, streamErr)
		}
		if closeErr := s.stream.Close(); closeErr != nil {
			return closeErr
		}
		delay := retryDelay(s.config, s.interval, streamErr)
		notifyRetry(s.ctx, s.config, RetryEvent{
			Attempt: s.attempts,
			Delay:   delay,
			Err:     streamErr,
		})
		if err := waitForRetry(s.ctx, delay); err != nil {
			return err
		}
		s.attempts++
		next, err := s.provider.Stream(s.ctx, s.request)
		if err == nil && next == nil {
			err = errProviderNilStream
		}
		if err == nil {
			s.stream = next
			s.interval = advanceRetryInterval(s.config, s.interval)
			return nil
		}
		if !s.provider.IsTransient(err) {
			return err
		}
		streamErr = err
	}
}

func advanceRetryInterval(config RetryConfig, interval time.Duration) time.Duration {
	interval = time.Duration(float64(interval) * config.Multiplier)
	if interval > config.MaxInterval {
		return config.MaxInterval
	}
	return interval
}

func (s *retryStream) Err() error {
	if s == nil {
		return nil
	}
	return s.err
}

func (s *retryStream) Close() error {
	if s == nil || s.closed {
		return nil
	}
	s.closed = true
	if s.stream == nil {
		return nil
	}
	return s.stream.Close()
}
