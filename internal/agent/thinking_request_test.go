package agent

import (
	"context"
	"errors"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessProviderRequestsUseConfiguredThinkingLevel(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	var effort string
	h := NewController(ControllerConfig{
		Session:  sess,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingXHigh,
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			effort = req.ReasoningEffort
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	if _, err := h.Prompt(context.Background(), "thinking"); err != nil {
		t.Fatal(err)
	}
	if effort != string(session.ThinkingXHigh) {
		t.Fatalf("request reasoning effort = %q, want %q", effort, session.ThinkingXHigh)
	}
}

func TestHarnessThinkingChangeIsDurableBeforeNextPrompt(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendThinkingLevelChange(context.Background(), session.ThinkingHigh); err != nil {
		t.Fatal(err)
	}
	var effort string
	h := NewController(ControllerConfig{
		Session:  sess,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingHigh,
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			effort = req.ReasoningEffort
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	if err := h.SetThinking(context.Background(), session.ThinkingLow); err != nil {
		t.Fatal(err)
	}
	if got := h.GetThinkingLevel(); got != session.ThinkingLow {
		t.Fatalf("live thinking level = %q, want low", got)
	}
	if _, err := h.Prompt(context.Background(), "use the new level"); err != nil {
		t.Fatal(err)
	}
	if effort != string(session.ThinkingLow) {
		t.Fatalf("request reasoning effort = %q, want low", effort)
	}
	snap, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if snap.Thinking != session.ThinkingLow {
		t.Fatalf("replayed thinking level = %q, want low", snap.Thinking)
	}
}

type thinkingFailureSession struct {
	session.Session
	fail bool
}

func (s *thinkingFailureSession) AppendThinkingLevelChange(ctx context.Context, level session.ThinkingLevel) (string, error) {
	if s.fail {
		return "", errors.New("injected thinking persistence failure")
	}
	return s.Session.AppendThinkingLevelChange(ctx, level)
}

func TestHarnessThinkingChangeFailureLeavesLiveStateUnchanged(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	if _, err := base.AppendThinkingLevelChange(context.Background(), session.ThinkingHigh); err != nil {
		t.Fatal(err)
	}
	failing := &thinkingFailureSession{Session: base, fail: true}
	h := NewController(ControllerConfig{
		Session:  failing,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingHigh,
	})
	defer h.Close()

	if err := h.SetThinking(context.Background(), session.ThinkingLow); err == nil {
		t.Fatal("thinking persistence unexpectedly succeeded")
	}
	if got := h.GetThinkingLevel(); got != session.ThinkingHigh {
		t.Fatalf("live thinking level = %q, want high", got)
	}
	snap, err := failing.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if snap.Thinking != session.ThinkingHigh {
		t.Fatalf("replayed thinking level = %q, want high", snap.Thinking)
	}
}

func TestHarnessActiveThinkingFailureRollsBackAndRetries(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	if _, err := base.AppendThinkingLevelChange(context.Background(), session.ThinkingHigh); err != nil {
		t.Fatal(err)
	}
	failing := &thinkingFailureSession{Session: base, fail: true}
	var harness *Controller
	var requests []string
	started := make(chan struct{})
	release := make(chan struct{})
	harness = NewController(ControllerConfig{
		Session:  failing,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingHigh,
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			requests = append(requests, req.ReasoningEffort)
			if len(requests) == 1 {
				if err := harness.SetThinking(ctx, session.ThinkingLow); err != nil {
					return nil, err
				}
				close(started)
				<-release
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer harness.Close()

	firstDone := make(chan error, 1)
	go func() {
		_, err := harness.Prompt(context.Background(), "first")
		firstDone <- err
	}()
	<-started
	close(release)
	if err := <-firstDone; err == nil {
		t.Fatal("first prompt unexpectedly succeeded")
	}
	if got := harness.GetThinkingLevel(); got != session.ThinkingHigh {
		t.Fatalf("thinking after failed active write = %q, want high", got)
	}

	failing.fail = false
	if _, err := harness.Prompt(context.Background(), "retry"); err != nil {
		t.Fatal(err)
	}
	if len(requests) != 2 || requests[1] != string(session.ThinkingLow) {
		t.Fatalf("provider reasoning efforts = %#v, want first high then low", requests)
	}
	if got := harness.GetThinkingLevel(); got != session.ThinkingLow {
		t.Fatalf("thinking after retry = %q, want low", got)
	}
}

func TestHarnessProviderRequestsLeaveAutoUnset(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	var effort string
	h := NewController(ControllerConfig{
		Session:  sess,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingAuto,
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			effort = req.ReasoningEffort
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	if _, err := h.Prompt(context.Background(), "use provider default"); err != nil {
		t.Fatal(err)
	}
	if effort != "" {
		t.Fatalf("auto request reasoning effort = %q, want empty", effort)
	}
}
