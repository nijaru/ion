package agent

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"strings"
	"sync"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

const eventSubscriberCapacity = 512

// EventStreamID distinguishes one runtime instance from every previous or
// replacement runtime. It is intentionally runtime transport metadata, not a
// session entry or provider protocol value.
type EventStreamID [16]byte

// EventCursor identifies the next event a subscriber expects.
type EventCursor struct {
	Stream EventStreamID
	Next   uint64
}

func (c EventCursor) zero() bool { return c.Stream == (EventStreamID{}) && c.Next == 0 }

// EventEnvelope carries one typed runtime event with stream identity and
// sequence. The runtime event hub serializes publication and assigns the
// sequence.
type EventEnvelope struct {
	Stream   EventStreamID
	Sequence uint64
	Event    session.Event
}

// ErrRuntimeClosed is now in state.go.

// QueueSnapshot is the immutable queued-input portion of a runtime snapshot.
type QueueSnapshot struct {
	Steer    []session.Message
	FollowUp []session.Message
	NextTurn []session.Message
}

// Texts returns the user-visible projection of each queue without exposing
// runtime-owned message storage to a frontend. The runtime snapshot remains
// the source of truth; callers receive independent strings only.
func (q QueueSnapshot) Texts() (steer, followUp, nextTurn []string) {
	return queueMessageTexts(q.Steer), queueMessageTexts(q.FollowUp), queueMessageTexts(q.NextTurn)
}

func queueMessageTexts(messages []session.Message) []string {
	if len(messages) == 0 {
		return nil
	}
	texts := make([]string, 0, len(messages))
	for _, message := range messages {
		if text := strings.TrimSpace(session.MessageText(message)); text != "" {
			texts = append(texts, text)
		}
	}
	if len(texts) == 0 {
		return nil
	}
	return texts
}

// RuntimeSnapshot is the authoritative renderable runtime projection returned
// when a subscription is opened or resynchronized. Branch is read at the
// captured leaf; active fields are ephemeral controller state. Pending
// approvals are included because an approval event can be missed during
// subscription recovery while the tool remains blocked on the broker.
type RuntimeSnapshot struct {
	Cursor    EventCursor
	Resynced  bool
	SessionID string
	LeafID    string
	Branch    []session.Entry
	Phase     Phase
	// ActiveTurnToken identifies the active turn for turn-scoped frontend
	// cancellation. It is zero when the runtime has no active turn.
	ActiveTurnToken  uint64
	Model            llm.Model
	Thinking         session.ThinkingLevel
	ActiveTools      []string
	Queues           QueueSnapshot
	PendingApprovals []session.ApprovalRequest
}

type eventSubscriptionState struct {
	mu     sync.Mutex
	ch     chan EventEnvelope
	err    error
	closed bool
}

// EventSubscription is one independent broadcast subscription. Each
// subscriber owns a bounded channel; a slow subscriber is detached and must
// resubscribe from a fresh RuntimeSnapshot instead of blocking the writer.
type EventSubscription struct {
	Snapshot RuntimeSnapshot
	Events   <-chan EventEnvelope

	hub   *eventHub
	state *eventSubscriptionState
	once  sync.Once
}

func (s *EventSubscription) Err() error {
	if s == nil || s.state == nil {
		return ErrRuntimeClosed
	}
	s.state.mu.Lock()
	defer s.state.mu.Unlock()
	return s.state.err
}

func (s *EventSubscription) Close() {
	if s == nil || s.hub == nil || s.state == nil {
		return
	}
	s.once.Do(func() { s.hub.remove(s.state) })
}

type eventHub struct {
	mu     sync.Mutex
	stream EventStreamID
	next   uint64
	subs   map[*eventSubscriptionState]struct{}
	closed bool
}

// Subscribe is now in runtime.go — it sends a SubscribeCmd through the
// typed command queue and calls subscribeDirect from the command goroutine.

func (h *Controller) subscribeDirect(ctx context.Context, after EventCursor) (*EventSubscription, error) {
	ctx, releaseContext := h.runtimeBoundContext(ctx)
	defer releaseContext()
	if h.eventHub == nil {
		return nil, ErrRuntimeClosed
	}
	for attempt := 0; attempt < 4; attempt++ {
		expected := h.eventHub.cursor()
		h.mu.Lock()
		if h.closed {
			h.mu.Unlock()
			return nil, ErrRuntimeClosed
		}
		pendingApprovals := []session.ApprovalRequest(nil)
		if h.approvals != nil {
			pendingApprovals = h.approvals.snapshot()
		}
		phase := h.phase
		if len(pendingApprovals) > 0 {
			// ApprovalBroker owns the pending decision while the turn worker is
			// blocked; expose the renderable phase even though the controller's
			// command-owned phase remains Streaming until the worker resumes.
			phase = PhaseAwaitingApproval
		}
		snapshot := RuntimeSnapshot{
			SessionID:        h.session.ID(),
			LeafID:           h.session.GetLeafID(),
			Phase:            phase,
			ActiveTurnToken:  h.activeTurnToken,
			Model:            h.model,
			Thinking:         h.thinking,
			ActiveTools:      append([]string(nil), h.active...),
			PendingApprovals: pendingApprovals,
			Queues: QueueSnapshot{
				Steer:    append([]session.Message(nil), h.steer...),
				FollowUp: append([]session.Message(nil), h.followUp...),
				NextTurn: append([]session.Message(nil), h.nextTurn...),
			},
		}
		leafID := snapshot.LeafID
		h.mu.Unlock()

		branch, err := h.session.BranchAt(ctx, leafID)
		if err != nil {
			return nil, fmt.Errorf("build runtime snapshot: %w", err)
		}
		persisted, err := session.ProjectContext(branch)
		if err != nil {
			return nil, fmt.Errorf("restore runtime state for snapshot: %w", err)
		}
		h.mu.Lock()
		if !phase.activeTurn() {
			if persisted.Thinking != "" && !h.thinkingPending {
				h.thinking = clampThinkingLevel(h.model, persisted.Thinking)
			}
			if persisted.ActiveToolsSet {
				h.active = append([]string(nil), persisted.ActiveTools...)
			}
			snapshot.Thinking = h.thinking
			snapshot.ActiveTools = append([]string(nil), h.active...)
		}
		h.mu.Unlock()
		snapshot.Branch = append([]session.Entry(nil), branch...)
		if h.eventHub.cursor() != expected || h.session.GetLeafID() != leafID {
			continue
		}
		sub, err := h.eventHub.subscribe(snapshot, expected, after)
		if errors.Is(err, ErrSnapshotChanged) {
			continue
		}
		return sub, err
	}
	return nil, ErrSnapshotChanged
}

func newEventHub() *eventHub {
	var stream EventStreamID
	if _, err := rand.Read(stream[:]); err != nil {
		// A zero stream is valid only as the absent cursor. If the platform
		// entropy source fails, keep construction deterministic but distinct
		// from the zero cursor.
		stream[0] = 1
	}
	return &eventHub{stream: stream, subs: make(map[*eventSubscriptionState]struct{})}
}

func (h *eventHub) cursor() EventCursor {
	h.mu.Lock()
	defer h.mu.Unlock()
	return EventCursor{Stream: h.stream, Next: h.next + 1}
}

func (h *eventHub) publish(event session.Event) {
	if h == nil || event == nil {
		return
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	h.next++
	envelope := EventEnvelope{Stream: h.stream, Sequence: h.next, Event: event}
	for sub := range h.subs {
		select {
		case sub.ch <- envelope:
		default:
			h.detachLocked(sub, ErrSubscriptionLagged)
		}
	}
}

func (h *eventHub) subscribe(snapshot RuntimeSnapshot, expected, after EventCursor) (*EventSubscription, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return nil, ErrRuntimeClosed
	}
	current := EventCursor{Stream: h.stream, Next: h.next + 1}
	if current != expected {
		return nil, ErrSnapshotChanged
	}
	snapshot.Cursor = current
	snapshot.Resynced = !after.zero() && after != current
	state := &eventSubscriptionState{ch: make(chan EventEnvelope, eventSubscriberCapacity)}
	h.subs[state] = struct{}{}
	return &EventSubscription{Snapshot: snapshot, Events: state.ch, hub: h, state: state}, nil
}

func (h *eventHub) remove(state *eventSubscriptionState) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	h.detachLocked(state, nil)
}

func (h *eventHub) detachLocked(state *eventSubscriptionState, err error) {
	if state == nil {
		return
	}
	if _, ok := h.subs[state]; !ok {
		return
	}
	delete(h.subs, state)
	state.mu.Lock()
	if !state.closed {
		state.err = err
		state.closed = true
		close(state.ch)
	}
	state.mu.Unlock()
}

func (h *eventHub) close() {
	if h == nil {
		return
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	h.closed = true
	for state := range h.subs {
		h.detachLocked(state, ErrRuntimeClosed)
	}
}
