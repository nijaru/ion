package agent

import (
	"context"
	"errors"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessInputQueuesAreBounded(t *testing.T) {
	h := NewController(ControllerConfig{
		Session:       newTestSession(t),
		QueueCapacity: 1,
	})
	defer h.Close()
	h.phase = PhaseStreaming

	if err := h.Steer("first"); err != nil {
		t.Fatalf("first steer: %v", err)
	}
	if err := h.Steer("second"); !errors.Is(err, ErrQueueFull) {
		t.Fatalf("second steer error = %v, want ErrQueueFull", err)
	}

	if err := h.FollowUp("first"); err != nil {
		t.Fatalf("first follow-up: %v", err)
	}
	if err := h.FollowUp("second"); !errors.Is(err, ErrQueueFull) {
		t.Fatalf("second follow-up error = %v, want ErrQueueFull", err)
	}

	if err := h.NextTurn("first"); err != nil {
		t.Fatalf("first next-turn: %v", err)
	}
	if err := h.NextTurn("second"); !errors.Is(err, ErrQueueFull) {
		t.Fatalf("second next-turn error = %v, want ErrQueueFull", err)
	}
}

type queuedTurnStream struct {
	ctx       context.Context
	gate      <-chan struct{}
	started   chan<- struct{}
	completed chan<- struct{}
	once      sync.Once
	sent      bool
}

func (s *queuedTurnStream) Next() (*llm.Chunk, bool) {
	if s.sent {
		return nil, false
	}
	if s.started != nil {
		s.once.Do(func() { close(s.started) })
	}
	if s.gate != nil {
		select {
		case <-s.gate:
		case <-s.ctx.Done():
			return nil, false
		}
	}
	s.sent = true
	if s.completed != nil {
		close(s.completed)
	}
	return &llm.Chunk{Content: "ok", StopReason: "stop"}, true
}

func (s *queuedTurnStream) Err() error   { return nil }
func (s *queuedTurnStream) Close() error { return nil }

func TestControllerNextTurnStartsQueuedPromptsAfterSettlement(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	firstStarted := make(chan struct{})
	releaseFirst := make(chan struct{})
	secondCompleted := make(chan struct{})
	thirdCompleted := make(chan struct{})
	var calls atomic.Int32

	h := NewController(ControllerConfig{
		Session: sess,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			switch calls.Add(1) {
			case 1:
				return &queuedTurnStream{ctx: ctx, gate: releaseFirst, started: firstStarted}, nil
			case 2:
				return &queuedTurnStream{ctx: ctx, completed: secondCompleted}, nil
			default:
				return &queuedTurnStream{ctx: ctx, completed: thirdCompleted}, nil
			}
		},
	})
	defer h.Close()

	firstDone := make(chan error, 1)
	go func() {
		_, err := h.Prompt(context.Background(), "first")
		firstDone <- err
	}()
	select {
	case <-firstStarted:
	case <-time.After(time.Second):
		t.Fatal("first turn did not start")
	}

	if err := h.NextTurn("second"); err != nil {
		t.Fatalf("queue second: %v", err)
	}
	if err := h.NextTurn("third"); err != nil {
		t.Fatalf("queue third: %v", err)
	}
	close(releaseFirst)

	select {
	case err := <-firstDone:
		if err != nil {
			t.Fatalf("first turn: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("first turn did not settle")
	}
	select {
	case <-secondCompleted:
	case <-time.After(time.Second):
		t.Fatal("queued second turn did not complete")
	}
	select {
	case <-thirdCompleted:
	case <-time.After(time.Second):
		t.Fatal("queued third turn did not complete")
	}
	if got := calls.Load(); got != 3 {
		t.Fatalf("provider calls = %d, want exactly three turns", got)
	}
}

func TestTerminalFailureClearsSteerAndFollowUpQueues(t *testing.T) {
	h := NewController(ControllerConfig{
		Session: newTestSession(t),
		Model:   llm.Model{ID: "test"},
	})
	defer h.Close()

	started := make(chan struct{})
	release := make(chan struct{})
	var calls atomic.Int32
	var stale atomic.Bool
	providerErr := errors.New("provider failed")
	h.stream = func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		if calls.Add(1) == 1 {
			close(started)
			select {
			case <-release:
			case <-ctx.Done():
				return nil, ctx.Err()
			}
			return &faultStream{streamErr: providerErr}, nil
		}
		for _, message := range req.Messages {
			if strings.Contains(message.Content, "stale") {
				stale.Store(true)
			}
		}
		return &mockStream{chunks: []*llm.Chunk{{Content: "recovered", StopReason: llm.StopReasonStop}}}, nil
	}

	firstDone := make(chan error, 1)
	go func() {
		_, err := h.Prompt(context.Background(), "first")
		firstDone <- err
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("first provider call did not start")
	}
	if err := h.Steer("stale steer"); err != nil {
		t.Fatalf("Steer: %v", err)
	}
	if err := h.FollowUp("stale follow-up"); err != nil {
		t.Fatalf("FollowUp: %v", err)
	}
	close(release)
	if err := <-firstDone; err == nil || !strings.Contains(err.Error(), providerErr.Error()) {
		t.Fatalf("first Prompt error = %v, want provider failure", err)
	}

	h.mu.Lock()
	if len(h.steer) != 0 || len(h.followUp) != 0 {
		t.Fatalf("terminal failure left queues: steer=%d follow_up=%d", len(h.steer), len(h.followUp))
	}
	h.mu.Unlock()
	if _, err := h.Prompt(context.Background(), "second"); err != nil {
		t.Fatalf("second Prompt: %v", err)
	}
	if stale.Load() {
		t.Fatal("failed-turn steer/follow-up leaked into the next provider request")
	}
}

func TestControllerSealsLateSteerAndFollowUpInput(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	defer h.Close()

	h.mu.Lock()
	h.phase = PhaseStreaming
	h.turnInputClosed = false
	h.mu.Unlock()
	config := h.buildLoopConfig(context.Background(), nil, nil)
	if got := config.DrainSteer(); got != nil {
		t.Fatalf("initial steer drain = %#v, want empty", got)
	}
	if got := config.DrainFollowUp(); got != nil {
		t.Fatalf("final follow-up drain = %#v, want empty", got)
	}
	if err := h.steerDirect("too late"); !errors.Is(err, ErrPhaseConflict) {
		t.Fatalf("late steer error = %v, want phase conflict", err)
	}
	if err := h.followUpDirect("too late"); !errors.Is(err, ErrPhaseConflict) {
		t.Fatalf("late follow-up error = %v, want phase conflict", err)
	}
}

func TestNextTurnCanQueueDuringTerminalPersistence(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	defer h.Close()

	h.mu.Lock()
	h.phase = PhasePersisting
	h.activeTurnID = "turn-in-progress"
	h.mu.Unlock()
	if err := h.NextTurn("after settlement"); err != nil {
		t.Fatalf("NextTurn during terminal persistence: %v", err)
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if len(h.nextTurn) != 1 {
		t.Fatalf("next-turn queue length = %d, want one", len(h.nextTurn))
	}
}

func TestAbortPendingPromptTokenClearsQueuedNextTurns(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t), QueueCapacity: 2})
	defer h.Close()

	token := h.reserveTurnToken()
	if err := h.nextTurnDirect("discard me"); err != nil {
		t.Fatalf("queue next turn: %v", err)
	}
	if _, _, err := h.AbortTurn(token); err != nil {
		t.Fatalf("abort pending prompt: %v", err)
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if len(h.nextTurn) != 0 {
		t.Fatalf("next-turn queue = %#v, want empty after pending abort", h.nextTurn)
	}
}

func TestAbortClearsRuntimeOwnedNextTurns(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t), QueueCapacity: 2})
	defer h.Close()
	h.phase = PhaseStreaming

	if err := h.NextTurn("discard me"); err != nil {
		t.Fatalf("queue next turn: %v", err)
	}
	if _, _, err := h.Abort(); err != nil {
		t.Fatalf("abort: %v", err)
	}
	if len(h.nextTurn) != 0 {
		t.Fatalf("next-turn queue = %#v, want empty after abort", h.nextTurn)
	}
}
