package canto

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/runtime"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/subagents"
)

type recordingAgent struct {
	id string

	mu        sync.Mutex
	histories [][]llm.Message
}

func (a *recordingAgent) ID() string {
	if a.id == "" {
		return "recording-agent"
	}
	return a.id
}

func (a *recordingAgent) Step(
	ctx context.Context,
	sess *csession.Session,
) (agent.StepResult, error) {
	return a.Turn(ctx, sess)
}

func (a *recordingAgent) Turn(
	ctx context.Context,
	sess *csession.Session,
) (agent.StepResult, error) {
	messages, err := sess.EffectiveMessages()
	if err != nil {
		return agent.StepResult{}, err
	}
	a.mu.Lock()
	a.histories = append(a.histories, append([]llm.Message(nil), messages...))
	a.mu.Unlock()
	return agent.StepResult{Content: "child summary"}, nil
}

func (a *recordingAgent) LastHistory() []llm.Message {
	a.mu.Lock()
	defer a.mu.Unlock()
	if len(a.histories) == 0 {
		return nil
	}
	return append([]llm.Message(nil), a.histories[len(a.histories)-1]...)
}

func TestSubagentToolSpecIncludesContextModes(t *testing.T) {
	tool := NewSubagentTool(nil, []subagents.Persona{{Name: "explorer"}})

	parameters, ok := tool.Spec().Parameters.(map[string]any)
	if !ok {
		t.Fatalf("parameters missing from spec: %#v", tool.Spec().Parameters)
	}
	properties, ok := parameters["properties"].(map[string]any)
	if !ok {
		t.Fatalf("properties missing from spec: %#v", parameters)
	}
	field, ok := properties["context_mode"].(map[string]any)
	if !ok {
		t.Fatalf("context_mode missing from spec: %#v", properties)
	}
	values, ok := field["enum"].([]string)
	if !ok {
		t.Fatalf("context_mode enum = %#v, want []string", field["enum"])
	}
	want := []string{"summary", "fork", "none"}
	if strings.Join(values, ",") != strings.Join(want, ",") {
		t.Fatalf("context_mode enum = %#v, want %#v", values, want)
	}
	if field["default"] != "summary" {
		t.Fatalf("context_mode default = %#v, want summary", field["default"])
	}
}

func TestSubagentInputChildSpecMapsContextModes(t *testing.T) {
	persona := subagents.Persona{Name: "explorer", ModelSlot: subagents.ModelSlotFast}
	child := &recordingAgent{id: "child"}

	for _, tt := range []struct {
		name    string
		args    string
		mode    csession.ChildMode
		context string
		initial string
	}{
		{
			name:    "summary default",
			args:    `{"agent":"explorer","task":"inspect","context":"focus on app"}`,
			mode:    csession.ChildModeHandoff,
			context: "focus on app",
		},
		{
			name:    "fork",
			args:    `{"agent":"explorer","task":"inspect","context":"focus on app","context_mode":"fork"}`,
			mode:    csession.ChildModeFork,
			context: "focus on app",
			initial: "Task: inspect\nContext: focus on app",
		},
		{
			name:    "none",
			args:    `{"agent":"explorer","task":"inspect","context_mode":"none"}`,
			mode:    csession.ChildModeFresh,
			context: "",
			initial: "Task: inspect",
		},
	} {
		t.Run(tt.name, func(t *testing.T) {
			input, err := parseSubagentInput(tt.args)
			if err != nil {
				t.Fatalf("parseSubagentInput: %v", err)
			}
			spec := input.childSpec("child-id", child, persona)
			if spec.Mode != tt.mode {
				t.Fatalf("child mode = %q, want %q", spec.Mode, tt.mode)
			}
			if spec.Context != tt.context {
				t.Fatalf("child context = %q, want %q", spec.Context, tt.context)
			}
			if spec.Metadata["context_mode"] != string(input.ContextMode) {
				t.Fatalf(
					"metadata context_mode = %#v, want %q",
					spec.Metadata["context_mode"],
					input.ContextMode,
				)
			}
			if tt.initial == "" {
				if len(spec.InitialMessages) != 0 {
					t.Fatalf("initial messages = %#v, want none", spec.InitialMessages)
				}
				return
			}
			if len(spec.InitialMessages) != 1 {
				t.Fatalf("initial message count = %d, want 1", len(spec.InitialMessages))
			}
			msg := spec.InitialMessages[0]
			if msg.Role != llm.RoleUser || msg.Content != tt.initial {
				t.Fatalf("initial message = %#v, want user %q", msg, tt.initial)
			}
		})
	}
}

func TestSubagentContextModeNoneRejectsExtraContext(t *testing.T) {
	_, err := parseSubagentInput(
		`{"agent":"explorer","task":"inspect","context":"should not pass","context_mode":"none"}`,
	)
	if err == nil || !strings.Contains(err.Error(), "context must be empty") {
		t.Fatalf("parseSubagentInput error = %v, want context rejection", err)
	}
}

func TestSubagentForkContextUsesProviderVisibleParentSnapshot(t *testing.T) {
	store, err := csession.NewSQLiteStore(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()

	parent := csession.New("parent-session").WithWriter(store)
	parentAt := time.Date(2026, 5, 2, 17, 0, 0, 0, time.UTC)
	parentEvent := csession.NewMessage(parent.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "parent fact",
	})
	parentEvent.Timestamp = parentAt
	if err := parent.Append(t.Context(), parentEvent); err != nil {
		t.Fatalf("append parent user: %v", err)
	}
	call := llm.Call{ID: "subagent-call", Type: "function"}
	call.Function.Name = "subagent"
	call.Function.Arguments = `{"agent":"explorer","task":"inspect","context_mode":"fork"}`
	if err := parent.Append(t.Context(), csession.NewMessage(parent.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append parent tool call: %v", err)
	}

	child := &recordingAgent{id: "child"}
	runner := runtime.NewRunner(store, &recordingAgent{id: "parent"})
	defer runner.Close()

	result, err := runner.Delegate(t.Context(), parent.ID(), runtime.ChildSpec{
		ID:              "child-fork",
		Agent:           child,
		Mode:            csession.ChildModeFork,
		Task:            "inspect",
		InitialMessages: []llm.Message{subagentTaskMessage("inspect", "")},
	})
	if err != nil {
		t.Fatalf("delegate: %v", err)
	}
	if result.Status != csession.ChildStatusCompleted {
		t.Fatalf("child status = %q, want completed", result.Status)
	}
	childSession, err := store.Load(t.Context(), result.Ref.SessionID)
	if err != nil {
		t.Fatalf("load child session: %v", err)
	}
	childEvents := childSession.Events()
	if len(childEvents) == 0 {
		t.Fatal("child session has no events")
	}
	origin, ok, err := childEvents[0].ForkOrigin()
	if err != nil {
		t.Fatalf("decode fork origin: %v", err)
	}
	if !ok || origin.EventID != parentEvent.ID.String() {
		t.Fatalf("child first event origin = %#v, ok=%v, want parent event %s", origin, ok, parentEvent.ID)
	}
	if !childEvents[0].Timestamp.Equal(parentAt) {
		t.Fatalf("child forked timestamp = %s, want %s", childEvents[0].Timestamp, parentAt)
	}

	messages := child.LastHistory()
	if !historyHasMessage(messages, llm.RoleUser, "parent fact") {
		t.Fatalf("child history missing parent visible history: %#v", messages)
	}
	if !historyHasMessage(messages, llm.RoleUser, "Task: inspect") {
		t.Fatalf("child history missing task message: %#v", messages)
	}
	for _, msg := range messages {
		if msg.Role == llm.RoleAssistant && len(msg.Calls) > 0 {
			t.Fatalf("child provider history includes parent in-flight tool call: %#v", messages)
		}
	}
}

func historyHasMessage(messages []llm.Message, role llm.Role, text string) bool {
	for _, msg := range messages {
		if msg.Role == role && strings.Contains(msg.Content, text) {
			return true
		}
	}
	return false
}
