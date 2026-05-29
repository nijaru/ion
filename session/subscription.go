package session

import (
	"context"
	goruntime "runtime"
	"sync"
)

const subscriberBufSize = 64

// subscriber is a single fan-out recipient.
// The mu guards ch against concurrent trySend and close calls.
type subscriber struct {
	mu     sync.Mutex
	ch     chan Event
	closed bool
}

// trySend delivers e to the subscriber without blocking.
// Safe to call concurrently with close.
func (sub *subscriber) trySend(e Event) {
	sub.mu.Lock()
	defer sub.mu.Unlock()
	if sub.closed {
		return
	}
	select {
	case sub.ch <- e:
	default: // slow subscriber; drop
	}
}

// close marks the subscriber done and closes the channel.
// Idempotent; safe to call concurrently with trySend.
func (sub *subscriber) close() {
	sub.mu.Lock()
	defer sub.mu.Unlock()
	if !sub.closed {
		sub.closed = true
		close(sub.ch)
	}
}

// Subscription is a live, lossy watch over newly appended session events.
//
// Close is idempotent and should be called when the caller is done consuming
// events. The runtime also attaches a best-effort cleanup so abandoned
// subscriptions do not permanently retain session subscribers, but callers
// must not rely on GC for timely cleanup.
type Subscription struct {
	state *subscriptionState
}

type subscriptionState struct {
	sess    *Session
	sub     *subscriber
	ch      chan Event
	once    sync.Once
	cleanup goruntime.Cleanup
}

func newSubscription(sess *Session) *Subscription {
	ch := make(chan Event, subscriberBufSize)
	sub := &subscriber{ch: ch}
	state := &subscriptionState{
		sess: sess,
		sub:  sub,
		ch:   ch,
	}
	watch := &Subscription{state: state}
	state.cleanup = goruntime.AddCleanup(watch, cleanupSubscription, state)

	sess.mu.Lock()
	sess.subscribers = append(sess.subscribers, sub)
	sess.mu.Unlock()

	return watch
}

func cleanupSubscription(state *subscriptionState) {
	state.close(false)
}

func (state *subscriptionState) close(stopCleanup bool) {
	state.once.Do(func() {
		if stopCleanup {
			state.cleanup.Stop()
		}
		state.sess.removeSubscriber(state.sub)
		state.sub.close()
	})
}

// Watch returns a live, buffered stream of events appended after this call.
//
// Slow consumers drop events rather than blocking Append. Prefer this API over
// Subscribe when you need a live UI, indexer, or observer and call Close when
// you are done.
func (s *Session) Watch(ctx context.Context) *Subscription {
	watch := newSubscription(s)
	if done := ctx.Done(); done != nil {
		go func() {
			<-done
			watch.state.close(true)
		}()
	}
	return watch
}

// HasWatchers returns true if the session has any live stream observers.
func (s *Session) HasWatchers() bool {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.subscribers) > 0 || len(s.observers) > 0
}

// Events returns the live event channel for this subscription.
func (s *Subscription) Events() <-chan Event {
	if s == nil || s.state == nil {
		return nil
	}
	return s.state.ch
}

// Close detaches the subscription and closes its event channel.
func (s *Subscription) Close() {
	if s == nil || s.state == nil {
		return
	}
	s.state.close(true)
}
