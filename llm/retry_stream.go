package llm

import (
	"context"
	"time"
)

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
		_ = s.stream.Close()
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
