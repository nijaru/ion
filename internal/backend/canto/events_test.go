package canto

import (
	"context"
	"errors"
	"math"
	"strings"
	"testing"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func translateSessionEvents(
	ctx context.Context,
	b *Backend,
	events <-chan csession.Event,
	turnID uint64,
) {
	for ev := range events {
		if b.translateEvent(ctx, ev, turnID) {
			return
		}
	}
}

func TestTranslateEventsCommitsAssistantFromMessageAdded(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewEvent("session-id", csession.MessageAdded, llm.Message{
		Role:      llm.RoleAssistant,
		Content:   "done",
		Reasoning: "brief reasoning",
	})
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev1 := receiveEvent(t, b.Session().Events())
	committed, ok := ev1.(ionsession.AgentMessage)
	if !ok {
		t.Fatalf("first event = %T, want AgentMessage", ev1)
	}
	if committed.Message != "done" || committed.Reasoning != "brief reasoning" {
		t.Fatalf("committed message = %#v", committed)
	}

	ev2 := receiveEvent(t, b.Session().Events())
	if _, ok := ev2.(ionsession.TurnFinished); !ok {
		t.Fatalf("second event = %T, want TurnFinished", ev2)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsCommitsUserFromMessageAdded(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewEvent("session-id", csession.MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "read README.md",
	})
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev1 := receiveEvent(t, b.Session().Events())
	committed, ok := ev1.(ionsession.UserMessage)
	if !ok {
		t.Fatalf("first event = %T, want UserMessage", ev1)
	}
	if committed.Message != "read README.md" {
		t.Fatalf("committed user message = %#v", committed)
	}

	ev2 := receiveEvent(t, b.Session().Events())
	if _, ok := ev2.(ionsession.TurnFinished); !ok {
		t.Fatalf("second event = %T, want TurnFinished", ev2)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsTurnCompletedDoesNotEmitEmptyAssistant(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev1 := receiveEvent(t, b.Session().Events())
	if _, ok := ev1.(ionsession.TurnFinished); !ok {
		t.Fatalf("first event = %T, want TurnFinished", ev1)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsClearsActiveTurnBeforeFinishedEvent(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true
	b.turn.cancel = func() {}

	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	translateSessionEvents(t.Context(), b, events, 7)

	if b.turn.active {
		t.Fatal("turn remained active after terminal event translation")
	}
	if b.turn.cancel != nil {
		t.Fatal("cancel func remained set after terminal event translation")
	}
	ev := receiveEvent(t, b.Session().Events())
	if _, ok := ev.(ionsession.TurnFinished); !ok {
		t.Fatalf("event = %T, want TurnFinished", ev)
	}
}

func TestTranslateEventsSuppressesCanceledTerminalError(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
		Error: context.Canceled.Error(),
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev1 := receiveEvent(t, b.Session().Events())
	if _, ok := ev1.(ionsession.TurnFinished); !ok {
		t.Fatalf("first event = %T, want TurnFinished", ev1)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsReportsDeadlineTerminalError(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
		Error: context.DeadlineExceeded.Error(),
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev1 := receiveEvent(t, b.Session().Events())
	errEvent, ok := ev1.(ionsession.Error)
	if !ok {
		t.Fatalf("first event = %T, want Error", ev1)
	}
	if !strings.Contains(errEvent.Err.Error(), context.DeadlineExceeded.Error()) {
		t.Fatalf("error = %v, want deadline exceeded", errEvent.Err)
	}

	ev2 := receiveEvent(t, b.Session().Events())
	if _, ok := ev2.(ionsession.TurnFinished); !ok {
		t.Fatalf("second event = %T, want TurnFinished", ev2)
	}
}

func TestFinishTurnWithErrorReportsDeadlineExceeded(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true
	b.turn.cancel = func() {}

	b.finishTurnWithError(7, context.DeadlineExceeded)

	if b.turn.active {
		t.Fatal("turn remained active after deadline terminal")
	}
	if b.turn.cancel != nil {
		t.Fatal("cancel func remained set after deadline terminal")
	}
	ev1 := receiveEvent(t, b.Session().Events())
	errEvent, ok := ev1.(ionsession.Error)
	if !ok {
		t.Fatalf("first event = %T, want Error", ev1)
	}
	if !strings.Contains(errEvent.Err.Error(), context.DeadlineExceeded.Error()) {
		t.Fatalf("error = %v, want deadline exceeded", errEvent.Err)
	}
	ev2 := receiveEvent(t, b.Session().Events())
	if _, ok := ev2.(ionsession.TurnFinished); !ok {
		t.Fatalf("second event = %T, want TurnFinished", ev2)
	}
	select {
	case ev := <-b.Session().Events():
		t.Fatalf("deadline terminal emitted extra event: %#v", ev)
	default:
	}
}

func TestTerminalErrorAfterCancelFinishesQuietly(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true
	b.turn.cancel = func() {}

	if err := b.Session().CancelTurn(t.Context()); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}
	assertNoBackendEvent(t, b)

	if !b.emitTurnError(7, ionsession.BaseNow(), errors.New("late provider error")) {
		t.Fatal("late terminal error did not settle canceled turn")
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("late terminal error did not finish canceled turn")
	}
	select {
	case ev := <-b.Session().Events():
		t.Fatalf("late terminal error emitted event after cancel: %#v", ev)
	default:
	}
}

func TestTranslateEventsSuppressesInactiveTurnEvents(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = false

	events := make(chan csession.Event, 4)
	events <- csession.NewEvent("session-id", csession.MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "late assistant",
	})
	events <- csession.NewToolStartedEvent("session-id", csession.ToolStartedData{
		ID:        "tool-call-1",
		Tool:      "bash",
		Arguments: "echo late",
	})
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "late output",
	})
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
		Error: context.Canceled.Error(),
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 7)

	select {
	case ev := <-b.Session().Events():
		t.Fatalf("inactive turn emitted event: %#v", ev)
	default:
	}
}

func TestTranslateRunEventSuppressesInactiveTurnChunk(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = false

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type:  cantofw.RunEventChunk,
		Chunk: llm.Chunk{Content: "late chunk"},
	}, 7)

	select {
	case ev := <-b.Session().Events():
		t.Fatalf("inactive turn emitted chunk event: %#v", ev)
	default:
	}
}

func TestTranslateRunEventEmitsThinkingDeltaFromReasoningChunk(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type:  cantofw.RunEventChunk,
		Chunk: llm.Chunk{Reasoning: "thinking through it"},
	}, 7)

	ev := receiveEvent(t, b.Session().Events())
	delta, ok := ev.(ionsession.ThinkingDelta)
	if !ok {
		t.Fatalf("event = %T, want ThinkingDelta", ev)
	}
	if delta.Delta != "thinking through it" {
		t.Fatalf("delta = %q, want thinking through it", delta.Delta)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateRunEventEmitsTokenUsageDeltas(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventChunk,
		Usage: &cantofw.RunUsage{
			Kind: cantofw.RunUsageProviderDelta,
			Delta: llm.Usage{
				InputTokens:  10,
				OutputTokens: 0,
				Cost:         0.01,
			},
		},
	}, 7)
	first, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatal("first event is not TokenUsage")
	}
	if first.Input != 10 || first.Output != 0 || first.Total != 10 || first.Cost != 0.01 {
		t.Fatalf("first usage = %#v, want 10/0/10/0.01", first)
	}

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventChunk,
		Usage: &cantofw.RunUsage{
			Kind: cantofw.RunUsageProviderDelta,
			Delta: llm.Usage{
				OutputTokens: 5,
				TotalTokens:  5,
				Cost:         0.005,
			},
		},
	}, 7)
	second, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatal("second event is not TokenUsage")
	}
	if second.Input != 0 || second.Output != 5 || second.Total != 5 ||
		math.Abs(second.Cost-0.005) > 1e-9 {
		t.Fatalf("second usage = %#v, want 0/5/5/0.005", second)
	}
}

func TestTranslateRunEventUsesProviderTotalUsageDelta(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventChunk,
		Usage: &cantofw.RunUsage{
			Kind: cantofw.RunUsageProviderDelta,
			Delta: llm.Usage{
				InputTokens:  10,
				OutputTokens: 2,
				TotalTokens:  20,
				Cost:         0.01,
			},
		},
	}, 7)
	first, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatal("first event is not TokenUsage")
	}
	if first.Input != 10 || first.Output != 2 || first.Total != 20 || first.Cost != 0.01 {
		t.Fatalf("first usage = %#v, want 10/2/20/0.01", first)
	}

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventChunk,
		Usage: &cantofw.RunUsage{
			Kind: cantofw.RunUsageProviderDelta,
			Delta: llm.Usage{
				OutputTokens: 3,
				TotalTokens:  4,
				Cost:         0.005,
			},
		},
	}, 7)
	second, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatal("second event is not TokenUsage")
	}
	if second.Input != 0 || second.Output != 3 || second.Total != 4 ||
		math.Abs(second.Cost-0.005) > 1e-9 {
		t.Fatalf("second usage = %#v, want 0/3/4/0.005", second)
	}
}

func TestTranslateRunEventUsesCantoUsageAfterToolCompleted(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventChunk,
		Usage: &cantofw.RunUsage{
			Kind: cantofw.RunUsageProviderDelta,
			Delta: llm.Usage{
				InputTokens:  10,
				OutputTokens: 2,
			},
		},
	}, 7)
	_ = receiveEvent(t, b.Session().Events())

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventSession,
		Event: csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
			ID:     "tool-call-1",
			Tool:   "bash",
			Output: "ok",
		}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:   cantofw.RunLifecycleTool,
			Status: cantofw.RunLifecycleCompleted,
			Tool: &cantofw.RunToolLifecycle{
				ID:     "tool-call-1",
				Name:   "bash",
				Output: "ok",
			},
		},
	}, 7)
	_ = receiveEvent(t, b.Session().Events())
	_ = receiveEvent(t, b.Session().Events())

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventChunk,
		Usage: &cantofw.RunUsage{
			Kind: cantofw.RunUsageProviderDelta,
			Delta: llm.Usage{
				InputTokens:  12,
				OutputTokens: 3,
			},
		},
	}, 7)
	next, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatal("next event is not TokenUsage")
	}
	if next.Input != 12 || next.Output != 3 {
		t.Fatalf("next usage = %#v, want new request total 12/3", next)
	}
}

func TestTranslateRunSessionEventEmitsTerminalUsageDeltaBeforeFinish(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	usage := &cantofw.RunUsage{
		Kind: cantofw.RunUsageTurn,
		Delta: llm.Usage{
			InputTokens:  3,
			OutputTokens: 4,
			TotalTokens:  7,
			Cost:         0.25,
		},
		Cumulative: llm.Usage{
			InputTokens:  3,
			OutputTokens: 4,
			TotalTokens:  7,
			Cost:         0.25,
		},
	}
	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventSession,
		Event: csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
			Usage: usage.Cumulative,
		}),
		Usage: usage,
		Lifecycle: &cantofw.RunLifecycle{
			Type:     cantofw.RunLifecycleTurn,
			Status:   cantofw.RunLifecycleCompleted,
			Terminal: true,
			Usage:    usage,
		},
	}, 7)

	msg, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatalf("first event = %T, want TokenUsage", msg)
	}
	if msg.Input != 3 || msg.Output != 4 || msg.Total != 7 || msg.Cost != 0.25 {
		t.Fatalf("terminal usage = %#v, want 3/4/7/0.25", msg)
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("second event is not TurnFinished")
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateRunResultEmitsTerminalUsageDeltaBeforeFinish(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventResult,
		Usage: &cantofw.RunUsage{
			Kind: cantofw.RunUsageTurn,
			Delta: llm.Usage{
				InputTokens:  5,
				OutputTokens: 6,
				TotalTokens:  11,
				Cost:         0.33,
			},
			Cumulative: llm.Usage{
				InputTokens:  5,
				OutputTokens: 6,
				TotalTokens:  11,
				Cost:         0.33,
			},
		},
	}, 7)

	msg, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatalf("first event = %T, want TokenUsage", msg)
	}
	if msg.Input != 5 || msg.Output != 6 || msg.Total != 11 || msg.Cost != 0.33 {
		t.Fatalf("terminal usage = %#v, want 5/6/11/0.33", msg)
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("second event is not TurnFinished")
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateRunEventProjectsCantoToolLifecycle(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventSession,
		Event: csession.NewToolStartedEvent("session-id", csession.ToolStartedData{
			ID:        "tool-call-1",
			Tool:      "bash",
			Arguments: "git status",
		}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:   cantofw.RunLifecycleTool,
			Status: cantofw.RunLifecycleStarted,
			Tool: &cantofw.RunToolLifecycle{
				ID:        "tool-call-1",
				Name:      "bash",
				Arguments: "git status",
			},
			ActiveTools: []cantofw.RunToolLifecycle{{
				ID:   "tool-call-1",
				Name: "bash",
			}},
		},
	}, 7)

	started, ok := receiveEvent(t, b.Session().Events()).(ionsession.ToolCallStarted)
	if !ok {
		t.Fatalf("first event = %T, want ToolCallStarted", started)
	}
	if started.ToolUseID != "tool-call-1" || started.ToolName != "bash" ||
		started.Args != "git status" {
		t.Fatalf("started tool = %#v", started)
	}
	status, ok := receiveEvent(t, b.Session().Events()).(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("second event = %T, want StatusChanged", status)
	}
	if status.Status != "Running bash..." {
		t.Fatalf("status = %q, want Running bash...", status.Status)
	}
	if !b.turn.hasActiveTool() {
		t.Fatal("Canto active tool snapshot did not mark the Ion turn active")
	}

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventSession,
		Event: csession.NewEvent("session-id", csession.ToolOutputDelta, map[string]string{
			"id":    "tool-call-1",
			"delta": "partial output",
		}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:   cantofw.RunLifecycleTool,
			Status: cantofw.RunLifecycleUpdated,
			Tool: &cantofw.RunToolLifecycle{
				ID:    "tool-call-1",
				Name:  "bash",
				Delta: "partial output",
			},
			ActiveTools: []cantofw.RunToolLifecycle{{
				ID:   "tool-call-1",
				Name: "bash",
			}},
		},
	}, 7)

	delta, ok := receiveEvent(t, b.Session().Events()).(ionsession.ToolOutputDelta)
	if !ok {
		t.Fatalf("third event = %T, want ToolOutputDelta", delta)
	}
	if delta.ToolUseID != "tool-call-1" || delta.Delta != "partial output" {
		t.Fatalf("tool delta = %#v", delta)
	}

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventSession,
		Event: csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
			ID:     "tool-call-1",
			Tool:   "bash",
			Output: "ok",
		}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:   cantofw.RunLifecycleTool,
			Status: cantofw.RunLifecycleCompleted,
			Tool: &cantofw.RunToolLifecycle{
				ID:     "tool-call-1",
				Name:   "bash",
				Output: "ok",
			},
		},
	}, 7)

	result, ok := receiveEvent(t, b.Session().Events()).(ionsession.ToolResult)
	if !ok {
		t.Fatalf("fourth event = %T, want ToolResult", result)
	}
	if result.ToolUseID != "tool-call-1" || result.ToolName != "bash" ||
		result.Result != "ok" || result.Error != nil {
		t.Fatalf("tool result = %#v", result)
	}
	status, ok = receiveEvent(t, b.Session().Events()).(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("fifth event = %T, want StatusChanged", status)
	}
	if status.Status != "Thinking..." {
		t.Fatalf("status = %q, want Thinking...", status.Status)
	}
	if b.turn.hasActiveTool() {
		t.Fatal("Canto active tool snapshot did not clear the Ion active tool")
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateRunEventProjectsCantoLifecycleStatus(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type:  cantofw.RunEventSession,
		Event: csession.NewCompactionStartedEvent("session-id", csession.CompactionStartedData{}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:   cantofw.RunLifecycleCompaction,
			Status: cantofw.RunLifecycleStarted,
		},
	}, 7)
	status, ok := receiveEvent(t, b.Session().Events()).(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("first event = %T, want StatusChanged", status)
	}
	if status.Status != "Compacting context..." {
		t.Fatalf("status = %q, want Compacting context...", status.Status)
	}

	b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type:  cantofw.RunEventSession,
		Event: csession.NewEscalationRetriedEvent("session-id", csession.EscalationRetriedData{}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:   cantofw.RunLifecycleRetry,
			Status: cantofw.RunLifecycleRetrying,
			Retry:  &cantofw.RunRetryLifecycle{Scope: "overflow_recovery"},
		},
	}, 7)
	status, ok = receiveEvent(t, b.Session().Events()).(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("second event = %T, want StatusChanged", status)
	}
	if status.Status != "Recovering from context overflow..." {
		t.Fatalf("status = %q, want overflow recovery", status.Status)
	}

	terminal := b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventSession,
		Event: csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
			Error: "context_length_exceeded",
		}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:     cantofw.RunLifecycleTurn,
			Status:   cantofw.RunLifecycleFailed,
			Terminal: true,
			Error:    "context_length_exceeded",
		},
	}, 7)
	if terminal {
		t.Fatal("context-overflow terminal event claimed the Ion turn")
	}

	terminal = b.translateRunEvent(t.Context(), cantofw.RunEvent{
		Type: cantofw.RunEventSession,
		Event: csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
			Error: "provider failed",
		}),
		Lifecycle: &cantofw.RunLifecycle{
			Type:     cantofw.RunLifecycleTurn,
			Status:   cantofw.RunLifecycleFailed,
			Terminal: true,
			Error:    "provider failed",
		},
	}, 7)
	if !terminal {
		t.Fatal("failed Canto turn lifecycle did not claim the Ion turn")
	}
	errEvent, ok := receiveEvent(t, b.Session().Events()).(ionsession.Error)
	if !ok {
		t.Fatalf("third event = %T, want Error", errEvent)
	}
	if errEvent.Err == nil || errEvent.Err.Error() != "provider failed" {
		t.Fatalf("error = %v, want provider failed", errEvent.Err)
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("fourth event is not TurnFinished")
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsPreservesToolUseID(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewToolStartedEvent("session-id", csession.ToolStartedData{
		ID:        "tool-call-1",
		Tool:      "bash",
		Arguments: "git status",
	})
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "ok",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev1 := receiveEvent(t, b.Session().Events())
	started, ok := ev1.(ionsession.ToolCallStarted)
	if !ok {
		t.Fatalf("first event = %T, want ToolCallStarted", ev1)
	}
	if started.ToolUseID != "tool-call-1" {
		t.Fatalf("started id = %q, want tool-call-1", started.ToolUseID)
	}
	_ = receiveEvent(t, b.Session().Events()) // status

	ev3 := receiveEvent(t, b.Session().Events())
	result, ok := ev3.(ionsession.ToolResult)
	if !ok {
		t.Fatalf("third event = %T, want ToolResult", ev3)
	}
	if result.ToolUseID != "tool-call-1" {
		t.Fatalf("result id = %q, want tool-call-1", result.ToolUseID)
	}
}

func TestTranslateEventsPreservesToolOutputDeltaID(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewEvent("session-id", csession.ToolOutputDelta, map[string]string{
		"id":    "tool-call-1",
		"tool":  "bash",
		"delta": "partial output",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev := receiveEvent(t, b.Session().Events())
	delta, ok := ev.(ionsession.ToolOutputDelta)
	if !ok {
		t.Fatalf("event = %T, want ToolOutputDelta", ev)
	}
	if delta.ToolUseID != "tool-call-1" {
		t.Fatalf("delta id = %q, want tool-call-1", delta.ToolUseID)
	}
	if delta.Delta != "partial output" {
		t.Fatalf("delta = %q, want partial output", delta.Delta)
	}
}

func TestTranslateEventsPreservesToolCompletedError(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "partial output\nError: exit status 1",
		Error:  "exit status 1",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	ev := receiveEvent(t, b.Session().Events())
	result, ok := ev.(ionsession.ToolResult)
	if !ok {
		t.Fatalf("event = %T, want ToolResult", ev)
	}
	if result.Error == nil || result.Error.Error() != "exit status 1" {
		t.Fatalf("tool result error = %v, want exit status 1", result.Error)
	}
	if result.Result != "partial output\nError: exit status 1" {
		t.Fatalf("tool result output = %q", result.Result)
	}
}

func TestTranslateEventsRestoresThinkingAfterLastActiveTool(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	events := make(chan csession.Event, 4)
	events <- csession.NewToolStartedEvent("session-id", csession.ToolStartedData{
		ID:        "tool-call-1",
		Tool:      "bash",
		Arguments: "echo one",
	})
	events <- csession.NewToolStartedEvent("session-id", csession.ToolStartedData{
		ID:        "tool-call-2",
		Tool:      "read",
		Arguments: "file.txt",
	})
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "one",
	})
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-2",
		Tool:   "read",
		Output: "two",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 7)

	_ = receiveEvent(t, b.Session().Events()) // first tool started
	_ = receiveEvent(t, b.Session().Events()) // first running status
	_ = receiveEvent(t, b.Session().Events()) // second tool started
	_ = receiveEvent(t, b.Session().Events()) // second running status
	_ = receiveEvent(t, b.Session().Events()) // first tool result; one tool remains active
	_ = receiveEvent(t, b.Session().Events()) // second tool result

	status, ok := receiveEvent(t, b.Session().Events()).(ionsession.StatusChanged)
	if !ok {
		t.Fatal("event is not StatusChanged")
	}
	if status.Status != "Thinking..." {
		t.Fatalf("status = %q, want Thinking...", status.Status)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsDoesNotRestoreThinkingForUntrackedTool(t *testing.T) {
	b := New()
	b.turn.seq = 7
	b.turn.active = true

	events := make(chan csession.Event, 1)
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "ok",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 7)

	ev := receiveEvent(t, b.Session().Events())
	_, ok := ev.(ionsession.ToolResult)
	if !ok {
		t.Fatalf("event = %T, want ToolResult", ev)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsUsesChildIDForSubagentRows(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewChildRequestedEvent("session-id", csession.ChildRequestedData{
		ChildID:        "explorer-123",
		ChildSessionID: "child-session",
		Task:           "inspect policy flow",
		AgentID:        "explorer",
		Mode:           csession.ChildModeHandoff,
	})
	events <- csession.NewChildStartedEvent("session-id", csession.ChildStartedData{
		ChildID:        "explorer-123",
		ChildSessionID: "child-session",
		AgentID:        "explorer",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	requested, ok := receiveEvent(t, b.Session().Events()).(ionsession.ChildRequested)
	if !ok {
		t.Fatal("first event is not ChildRequested")
	}
	if requested.AgentName != "explorer-123" {
		t.Fatalf("requested agent name = %q, want child id", requested.AgentName)
	}
	_ = receiveEvent(t, b.Session().Events()) // request status

	started, ok := receiveEvent(t, b.Session().Events()).(ionsession.ChildStarted)
	if !ok {
		t.Fatal("third event is not ChildStarted")
	}
	if started.AgentName != "explorer-123" {
		t.Fatalf("started agent name = %q, want child id", started.AgentName)
	}
}

func TestTranslateEventsChildProgressStatusOnlySkipsEmptyDelta(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewChildProgressedEvent("session-id", csession.ChildProgressedData{
		ChildID: "explorer-123",
		Status:  "waiting for approval",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	status, ok := receiveEvent(t, b.Session().Events()).(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("event = %T, want StatusChanged", status)
	}
	if !strings.Contains(status.Status, "waiting for approval") {
		t.Fatalf("status = %q, want child progress status", status.Status)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsChildTerminalDoesNotEmitReadyStatus(t *testing.T) {
	tests := []struct {
		name  string
		event csession.Event
		want  string
	}{
		{
			name: "completed",
			event: csession.NewChildCompletedEvent("session-id", csession.ChildCompletedData{
				ChildID: "explorer-123",
				Summary: "done",
			}),
			want: "completed",
		},
		{
			name: "failed",
			event: csession.NewChildFailedEvent("session-id", csession.ChildFailedData{
				ChildID: "explorer-123",
				Error:   "boom",
			}),
			want: "failed",
		},
		{
			name: "canceled",
			event: csession.NewChildCanceledEvent("session-id", csession.ChildCanceledData{
				ChildID: "explorer-123",
				Reason:  "user",
			}),
			want: "canceled",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			b := New()
			events := make(chan csession.Event, 1)
			events <- tt.event
			close(events)

			translateSessionEvents(t.Context(), b, events, 0)

			ev := receiveEvent(t, b.Session().Events())
			switch tt.want {
			case "completed":
				if _, ok := ev.(ionsession.ChildCompleted); !ok {
					t.Fatalf("event = %T, want ChildCompleted", ev)
				}
			case "failed":
				if _, ok := ev.(ionsession.ChildFailed); !ok {
					t.Fatalf("event = %T, want ChildFailed", ev)
				}
			case "canceled":
				if _, ok := ev.(ionsession.ChildCanceled); !ok {
					t.Fatalf("event = %T, want ChildCanceled", ev)
				}
			}
			assertNoBackendEvent(t, b)
		})
	}
}

func TestTranslateEventsChildCanceledIsNotFailure(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewChildCanceledEvent("session-id", csession.ChildCanceledData{
		ChildID: "explorer-123",
		Reason:  "user stopped it",
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	canceled, ok := receiveEvent(t, b.Session().Events()).(ionsession.ChildCanceled)
	if !ok {
		t.Fatalf("event = %T, want ChildCanceled", canceled)
	}
	if canceled.AgentName != "explorer-123" {
		t.Fatalf("agent = %q, want child id", canceled.AgentName)
	}
	if canceled.Reason != "user stopped it" {
		t.Fatalf("reason = %q, want cancellation reason", canceled.Reason)
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateEventsChildCompletedEmitsUsage(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewChildCompletedEvent("session-id", csession.ChildCompletedData{
		ChildID: "explorer-123",
		Summary: "done",
		Usage: llm.Usage{
			InputTokens:  12,
			OutputTokens: 5,
			TotalTokens:  23,
			Cost:         0.0042,
		},
	})
	close(events)

	translateSessionEvents(t.Context(), b, events, 0)

	completed, ok := receiveEvent(t, b.Session().Events()).(ionsession.ChildCompleted)
	if !ok {
		t.Fatalf("first event = %T, want ChildCompleted", completed)
	}
	if completed.AgentName != "explorer-123" || completed.Result != "done" {
		t.Fatalf("completed child = %#v", completed)
	}

	usage, ok := receiveEvent(t, b.Session().Events()).(ionsession.TokenUsage)
	if !ok {
		t.Fatalf("second event = %T, want TokenUsage", usage)
	}
	if usage.Input != 12 || usage.Output != 5 || usage.Total != 23 || usage.Cost != 0.0042 {
		t.Fatalf("token usage = %#v", usage)
	}
	assertNoBackendEvent(t, b)
}

func assertNoBackendEvent(t *testing.T, b *Backend) {
	t.Helper()
	select {
	case ev := <-b.Session().Events():
		t.Fatalf("unexpected backend event: %#v", ev)
	default:
	}
}
