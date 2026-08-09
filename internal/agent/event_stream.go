package agent

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"slices"
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

// ActiveToolSnapshot is the bounded, renderable identity of a tool call that
// is currently executing. Tool progress is intentionally not copied here:
// ToolPartial is an open-ended provider/tool value and the next event remains
// the authoritative update for subscribers that are not resynchronizing.
type ActiveToolSnapshot struct {
	ToolCallID string
	Name       string
	Args       []byte
}

// ActiveTurnSnapshot contains the in-flight state that is not necessarily in
// the selected durable branch yet. It lets a lagged frontend rebuild the
// visible assistant draft and running-tool indicators without treating UI
// state as a second persistence owner.
type ActiveTurnSnapshot struct {
	Assistant          *session.AssistantMessage
	AssistantCommitted bool
	AssistantInBranch  bool
	Tools              []ActiveToolSnapshot
}

// RuntimeSnapshot is the authoritative renderable runtime projection returned
// when a subscription is opened or resynchronized. Branch is read at the
// captured leaf, or from the active durable turn while one is running; active
// fields are ephemeral controller state. Pending
// approvals are included because an approval event can be missed during
// subscription recovery while the tool remains blocked on the broker.
type RuntimeSnapshot struct {
	Cursor     EventCursor
	Resynced   bool
	SessionID  string
	LeafID     string
	Branch     []session.Entry
	ActiveTurn ActiveTurnSnapshot
	Failure    string
	Phase      Phase
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

// observeRuntimeEventLocked records only the bounded render state needed to
// rebuild an active turn after subscription lag. It runs on the controller
// boundary; messages are copied before publication because provider streams
// may continue mutating their accumulated assistant value after a
// MessageUpdate returns. The caller must hold c.mu.
func (c *Controller) observeRuntimeEventLocked(event session.Event, persisted bool) {
	switch event := event.(type) {
	case session.AgentStart, session.TurnStart:
		c.activeAssistant = nil
		c.activeAssistantCommitted = false
		c.activeTools = nil
	case session.MessageStart:
		if assistant, ok := event.Message.(*session.AssistantMessage); ok {
			c.activeAssistant = cloneAssistantForSnapshot(assistant)
			c.activeAssistantCommitted = false
		}
	case session.MessageUpdate:
		if assistant, ok := event.Message.(*session.AssistantMessage); ok {
			c.activeAssistant = cloneAssistantForSnapshot(assistant)
			c.activeAssistantCommitted = false
		}
	case session.MessageEnd:
		if assistant, ok := event.Message.(*session.AssistantMessage); ok {
			c.activeAssistant = cloneAssistantForSnapshot(assistant)
			c.activeAssistantCommitted = persisted
		}
	case session.ToolExecStart:
		if c.activeTools == nil {
			c.activeTools = make(map[string]ActiveToolSnapshot)
		}
		c.activeTools[event.ToolCallID] = ActiveToolSnapshot{
			ToolCallID: event.ToolCallID,
			Name:       event.Name,
			Args:       append([]byte(nil), event.Args...),
		}
	case session.ToolExecEnd:
		delete(c.activeTools, event.ToolCallID)
	case session.Settled:
		c.activeAssistant = nil
		c.activeAssistantCommitted = false
		c.activeTools = nil
	}
}

func cloneMessagesForSnapshot(messages []session.Message) []session.Message {
	if len(messages) == 0 {
		return nil
	}
	cloned := make([]session.Message, len(messages))
	for i, message := range messages {
		cloned[i] = cloneMessageForSnapshot(message)
	}
	return cloned
}

func cloneMessageForSnapshot(message session.Message) session.Message {
	switch message := message.(type) {
	case *session.UserMessage:
		if message == nil {
			return (*session.UserMessage)(nil)
		}
		cloned := *message
		cloned.Content = cloneContentsForSnapshot(message.Content)
		return &cloned
	case *session.AssistantMessage:
		return cloneAssistantForSnapshot(message)
	case *session.ToolResultMessage:
		if message == nil {
			return (*session.ToolResultMessage)(nil)
		}
		cloned := *message
		cloned.Content = cloneContentsForSnapshot(message.Content)
		cloned.Details = append([]byte(nil), message.Details...)
		return &cloned
	case *session.CustomMessage:
		if message == nil {
			return (*session.CustomMessage)(nil)
		}
		cloned := *message
		cloned.Content = cloneContentsForSnapshot(message.Content)
		cloned.Details = append([]byte(nil), message.Details...)
		return &cloned
	default:
		return nil
	}
}

func cloneContentsForSnapshot(contents []session.Content) []session.Content {
	if len(contents) == 0 {
		return nil
	}
	cloned := make([]session.Content, len(contents))
	for i, content := range contents {
		switch content := content.(type) {
		case session.TextContent:
			cloned[i] = content
		case session.ThinkingContent:
			cloned[i] = content
		case session.ImageContent:
			image := content
			image.Data = append([]byte(nil), content.Data...)
			cloned[i] = image
		case *session.ToolCall:
			if content == nil {
				cloned[i] = (*session.ToolCall)(nil)
				continue
			}
			call := *content
			call.Arguments = cloneJSONMap(content.Arguments)
			cloned[i] = &call
		default:
			cloned[i] = nil
		}
	}
	return cloned
}

func cloneAssistantForSnapshot(assistant *session.AssistantMessage) *session.AssistantMessage {
	if assistant == nil {
		return nil
	}
	clone := *assistant
	clone.Content = make([]session.Content, 0, len(assistant.Content))
	for _, content := range assistant.Content {
		switch content := content.(type) {
		case session.TextContent:
			clone.Content = append(clone.Content, content)
		case session.ThinkingContent:
			clone.Content = append(clone.Content, content)
		case session.ImageContent:
			image := content
			image.Data = append([]byte(nil), content.Data...)
			clone.Content = append(clone.Content, image)
		case *session.ToolCall:
			if content == nil {
				clone.Content = append(clone.Content, (*session.ToolCall)(nil))
				continue
			}
			call := *content
			call.Arguments = cloneJSONMap(content.Arguments)
			clone.Content = append(clone.Content, &call)
		default:
			clone.Content = append(clone.Content, content)
		}
	}
	return &clone
}

func cloneJSONMap(values map[string]any) map[string]any {
	if values == nil {
		return nil
	}
	clone := make(map[string]any, len(values))
	for key, value := range values {
		clone[key] = cloneJSONValue(value)
	}
	return clone
}

func cloneJSONValue(value any) any {
	switch value := value.(type) {
	case map[string]any:
		return cloneJSONMap(value)
	case []any:
		clone := make([]any, len(value))
		for i, item := range value {
			clone[i] = cloneJSONValue(item)
		}
		return clone
	default:
		return value
	}
}

func branchContainsAssistant(entries []session.Entry, assistant *session.AssistantMessage) bool {
	if assistant == nil {
		return false
	}
	for i := len(entries) - 1; i >= 0; i-- {
		entry, ok := entries[i].(*session.MessageEntry)
		if !ok {
			continue
		}
		candidate, ok := entry.Message.(*session.AssistantMessage)
		if !ok {
			continue
		}
		if assistant.ResponseID != "" && candidate.ResponseID == assistant.ResponseID {
			return true
		}
		if candidate.Timestamp.UnixMilli() == assistant.Timestamp.UnixMilli() &&
			candidate.StopReason == assistant.StopReason &&
			candidate.Error == assistant.Error && session.MessageText(candidate) == session.MessageText(assistant) {
			return true
		}
	}
	return false
}

func (c *Controller) activeTurnSnapshotLocked() ActiveTurnSnapshot {
	snapshot := ActiveTurnSnapshot{
		Assistant:          cloneAssistantForSnapshot(c.activeAssistant),
		AssistantCommitted: c.activeAssistantCommitted,
	}
	if len(c.activeTools) == 0 {
		return snapshot
	}
	snapshot.Tools = make([]ActiveToolSnapshot, 0, len(c.activeTools))
	for _, tool := range c.activeTools {
		tool.Args = append([]byte(nil), tool.Args...)
		snapshot.Tools = append(snapshot.Tools, tool)
	}
	slices.SortFunc(snapshot.Tools, func(left, right ActiveToolSnapshot) int {
		return strings.Compare(left.ToolCallID, right.ToolCallID)
	})
	return snapshot
}

func (h *Controller) subscribeDirect(ctx context.Context, after EventCursor) (*EventSubscription, error) {
	ctx, releaseContext := h.runtimeBoundContext(ctx)
	defer releaseContext()
	if h.eventHub == nil {
		return nil, ErrRuntimeClosed
	}
	for attempt := 0; attempt < 4; attempt++ {
		h.mu.Lock()
		if h.runtimeBusy {
			busyDone := h.runtimeBusyDone
			h.mu.Unlock()
			if busyDone == nil {
				return nil, ErrSnapshotChanged
			}
			select {
			case <-busyDone:
				continue
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		expected := h.eventHub.cursor()
		if h.closed {
			h.mu.Unlock()
			return nil, ErrRuntimeClosed
		}
		pendingApprovals := []session.ApprovalRequest(nil)
		if h.approvals != nil {
			pendingApprovals = h.approvals.snapshot()
		}
		phase := h.phase
		activeTurnID := h.activeTurnID
		activeTurn := h.activeTurnSnapshotLocked()
		snapshotRevision := h.snapshotRevision
		if len(pendingApprovals) > 0 {
			// ApprovalBroker owns the pending decision while the turn worker is
			// blocked; expose the renderable phase even though the controller's
			// command-owned phase remains Streaming until the worker resumes.
			phase = PhaseAwaitingApproval
		}
		snapshot := RuntimeSnapshot{
			SessionID:        h.session.ID(),
			LeafID:           h.session.GetLeafID(),
			ActiveTurn:       activeTurn,
			Failure:          h.runtimeFailure,
			Phase:            phase,
			ActiveTurnToken:  h.activeTurnToken,
			Model:            h.model,
			Thinking:         h.thinking,
			ActiveTools:      append([]string(nil), h.active...),
			PendingApprovals: pendingApprovals,
			Queues: QueueSnapshot{
				Steer:    cloneMessagesForSnapshot(h.steer),
				FollowUp: cloneMessagesForSnapshot(h.followUp),
				NextTurn: cloneMessagesForSnapshot(h.nextTurn),
			},
		}
		leafID := snapshot.LeafID
		h.mu.Unlock()

		var branch []session.Entry
		var err error
		if activeTurnID != "" && h.durable != nil {
			// Ordinary BranchAt intentionally hides an uncommitted turn. The
			// active runtime snapshot is the one consumer that must include its
			// durable staged entries so a lagged frontend can rebuild the live
			// transcript before TurnCommit.
			branch, err = h.durable.TurnBranch(ctx, activeTurnID)
			if errors.Is(err, session.ErrTurnState) {
				// A terminal abort can clear the durable turn before the
				// controller publishes Settled. The committed branch remains the
				// authoritative snapshot for that short lifecycle boundary.
				branch, err = h.session.BranchAt(ctx, leafID)
			}
		} else {
			branch, err = h.session.BranchAt(ctx, leafID)
		}
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
		if snapshot.ActiveTurn.AssistantCommitted {
			snapshot.ActiveTurn.AssistantInBranch = branchContainsAssistant(
				branch, snapshot.ActiveTurn.Assistant,
			)
		}
		h.mu.Lock()
		turnChanged := h.activeTurnID != activeTurnID
		revisionChanged := h.snapshotRevision != snapshotRevision
		currentCursor := h.eventHub.cursor()
		currentLeaf := h.session.GetLeafID()
		busy := h.runtimeBusy
		if currentCursor != expected || currentLeaf != leafID || turnChanged || revisionChanged || busy {
			h.mu.Unlock()
			continue
		}
		sub, err := h.eventHub.subscribe(snapshot, expected, after)
		h.mu.Unlock()
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
