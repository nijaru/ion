package main

import (
	"context"
	"testing"

	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/session"
)

type closeStorageSession struct {
	id     string
	closed int
	events chan session.Event
}

func (s *closeStorageSession) ID() string                         { return s.id }
func (s *closeStorageSession) Meta() session.Metadata             { return session.Metadata{ID: s.id} }
func (s *closeStorageSession) SessionName(context.Context) string { return "" }
func (s *closeStorageSession) BuildContext(context.Context) (session.ContextSnapshot, error) {
	return session.ContextSnapshot{}, nil
}
func (s *closeStorageSession) Branch(context.Context) ([]session.Entry, error) { return nil, nil }
func (s *closeStorageSession) AppendMessage(context.Context, session.Message) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendModelChange(context.Context, string, string) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendThinkingLevelChange(context.Context, session.ThinkingLevel) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendActiveToolsChange(context.Context, []string) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendCompaction(context.Context, session.CompactionData) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendBranchSummary(context.Context, session.BranchSummaryData) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendLabel(context.Context, string, string) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendSessionInfo(context.Context, string) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendCustom(context.Context, *session.CustomEntry) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendLeaf(context.Context, string) (string, error) {
	return "", nil
}
func (s *closeStorageSession) AppendCustomMessage(context.Context, *session.CustomMessageEntry) (string, error) {
	return "", nil
}
func (s *closeStorageSession) GetLabel(context.Context, string) (string, error) {
	return "", nil
}
func (s *closeStorageSession) Append(context.Context, session.Entry) (string, error) {
	return "", nil
}
func (s *closeStorageSession) SubmitTurn(context.Context, string) error { return nil }
func (s *closeStorageSession) CancelTurn(context.Context) error         { return nil }
func (s *closeStorageSession) Events() <-chan session.Event             { return s.events }
func (s *closeStorageSession) EventSender() chan session.Event          { return s.events }
func (s *closeStorageSession) GetEntry(context.Context, string) (session.Entry, error) {
	return nil, nil
}
func (s *closeStorageSession) GetLeafID() string      { return "" }
func (s *closeStorageSession) SetLeafID(string) error { return nil }
func (s *closeStorageSession) MoveTo(context.Context, string, *session.BranchSummaryData) (string, error) {
	return "", nil
}
func (s *closeStorageSession) Entries(context.Context) ([]session.Entry, error) { return nil, nil }
func (s *closeStorageSession) Usage(context.Context) (session.Usage, error) {
	return session.Usage{}, nil
}
func (s *closeStorageSession) Close() error {
	s.closed++
	return nil
}

type providerBackend struct {
	provider string
	model    string
}

func (b providerBackend) Name() string             { return "provider-test" }
func (b providerBackend) Provider() string         { return b.provider }
func (b providerBackend) Model() string            { return b.model }
func (b providerBackend) ContextLimit() int        { return 0 }
func (b providerBackend) Bootstrap() app.Bootstrap { return app.Bootstrap{} }
func (b providerBackend) Session() session.Session { return nil }

func TestRuntimeHandlesForCloseUsesFinalAppRuntime(t *testing.T) {
	startupAgent := &printSession{events: make(chan session.Event)}
	currentAgent := &printSession{events: make(chan session.Event)}
	currentStorage := &closeStorageSession{id: "current", events: make(chan session.Event)}
	final := app.Model{
		Model: app.ModelState{
			Runner:  currentAgent,
			Storage: currentStorage,
		},
	}

	agent := runtimeHandlesForClose(&final, startupAgent)
	if agent != currentAgent {
		t.Fatalf("agent = %#v, want current runtime agent", agent)
	}
}

func TestRuntimeHandlesForCloseFallsBackForNonAppModel(t *testing.T) {
	startupAgent := &printSession{events: make(chan session.Event)}
	agent := runtimeHandlesForClose(nil, startupAgent)
	if agent != startupAgent {
		t.Fatalf("agent = %#v, want fallback agent", agent)
	}
}

func TestCloseRuntimeOpenErrorClosesPartialHandles(t *testing.T) {
	agent := &printSession{events: make(chan session.Event)}
	storageSession := &closeStorageSession{id: "partial", events: make(chan session.Event)}

	err := closeRuntimeOpenError(
		"backend initialization error",
		context.Canceled,
		agent,
		storageSession,
	)
	if err == nil {
		t.Fatal("closeRuntimeOpenError returned nil")
	}
	if agent.closed != 1 {
		t.Fatalf("agent closed = %d, want 1", agent.closed)
	}
	if storageSession.closed != 1 {
		t.Fatalf("storage closed = %d, want 1", storageSession.closed)
	}
	if got := err.Error(); got != "backend initialization error: context canceled" {
		t.Fatalf("error = %q, want labeled context cancellation", got)
	}
}

func TestStartupProviderMissing(t *testing.T) {
	if !startupProviderMissing(providerBackend{}) {
		t.Fatal("empty provider should need startup setup")
	}
	if startupProviderMissing(providerBackend{provider: "openai"}) {
		t.Fatal("configured provider should not need startup setup")
	}
	if startupProviderMissing(nil) {
		t.Fatal("nil backend should not need startup setup")
	}
}

func TestStartupModelMissing(t *testing.T) {
	if startupModelMissing(providerBackend{}) {
		t.Fatal("empty provider should not need model setup")
	}
	if !startupModelMissing(providerBackend{provider: "openrouter"}) {
		t.Fatal("configured provider without model should need model setup")
	}
	if startupModelMissing(providerBackend{provider: "openrouter", model: "model-a"}) {
		t.Fatal("configured provider and model should not need model setup")
	}
	if startupModelMissing(nil) {
		t.Fatal("nil backend should not need model setup")
	}
}
