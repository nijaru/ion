package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"errors"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/internal/core"
)

func TestSwitchReturnsAcceptedRuntimeAndPreservesTargetSession(t *testing.T) {
	oldSession := &fakeSession{id: "session-1"}
	oldStorage := &fakeStorage{id: "session-1"}
	newSession := &fakeSession{id: "session-1"}
	newStorage := &fakeStorage{id: "session-1", branch: "feature/runtime"}
	var targetSessionID string

	result, err := core.Switch(t.Context(), core.SwitchInput{
		Switcher: func(
			ctx context.Context,
			cfg *config.Config,
			sessionID string,
		) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			targetSessionID = sessionID
			return fakeBackend{
				provider: cfg.Provider,
				model:    cfg.Model,
				status:   "ready",
				session:  newSession,
			}, newSession, newStorage, nil
		},
		Transition: core.NewTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			core.PresetPrimary,
			"",
		).WithActivePresetPersistence(),
		Current:         core.Handles{Session: oldSession, Storage: oldStorage},
		PreserveSession: true,
		SaveState:       func(config.RuntimeStateUpdate) error { return nil },
	})
	if err != nil {
		t.Fatalf("switch: %v", err)
	}
	if targetSessionID != "session-1" {
		t.Fatalf("target session = %q, want preserved session", targetSessionID)
	}
	if result.Previous.Session != oldSession || result.Previous.Storage != oldStorage {
		t.Fatalf("previous handles = %#v, want current handles", result.Previous)
	}
	if result.Runtime.Handles.Session != newSession ||
		result.Runtime.Handles.Storage != newStorage {
		t.Fatalf("accepted handles = %#v, want new handles", result.Runtime.Handles)
	}
	snapshot := result.Runtime.Transition.Snapshot
	if snapshot.Provider != "openai" ||
		snapshot.Model != "gpt-4.1" ||
		snapshot.Status != "ready" ||
		snapshot.SessionID != "session-1" ||
		!snapshot.Materialized {
		t.Fatalf("snapshot = %#v, want accepted runtime state", snapshot)
	}
	if oldSession.cancels != 0 {
		t.Fatalf(
			"old session cancels = %d, want 0 before accepted switch is applied",
			oldSession.cancels,
		)
	}
}

func TestSwitchLeavesCurrentRuntimeUntouchedWhenOpenFails(t *testing.T) {
	openErr := errors.New("provider unavailable")
	oldSession := &fakeSession{id: "old"}
	oldStorage := &fakeStorage{id: "old"}

	_, err := core.Switch(t.Context(), core.SwitchInput{
		Switcher: func(
			context.Context,
			*config.Config,
			string,
		) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return nil, nil, nil, openErr
		},
		Transition: core.NewTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			core.PresetPrimary,
			"",
		).WithStatePersistence(),
		Current:   core.Handles{Session: oldSession, Storage: oldStorage},
		SaveState: func(config.RuntimeStateUpdate) error { return nil },
	})
	if !errors.Is(err, openErr) {
		t.Fatalf("switch error = %v, want open error", err)
	}
	if oldSession.cancels != 0 {
		t.Fatalf("old session cancels = %d, want 0 after failed open", oldSession.cancels)
	}
	if oldSession.closed {
		t.Fatal("old session was closed after failed open")
	}
	if oldStorage.closed {
		t.Fatal("old storage was closed after failed open")
	}
}

func TestSwitchClosesNewHandlesOnPersistFailure(t *testing.T) {
	persistErr := errors.New("state file unavailable")
	oldSession := &fakeSession{id: "old"}
	oldStorage := &fakeStorage{id: "old"}
	newSession := &fakeSession{id: "new"}
	newStorage := &fakeStorage{id: "new"}

	_, err := core.Switch(t.Context(), core.SwitchInput{
		Switcher: func(
			context.Context,
			*config.Config,
			string,
		) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return fakeBackend{session: newSession}, newSession, newStorage, nil
		},
		Transition: core.NewTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			core.PresetPrimary,
			"",
		).WithStatePersistence(),
		Current:   core.Handles{Session: oldSession, Storage: oldStorage},
		SaveState: func(config.RuntimeStateUpdate) error { return persistErr },
	})
	if !errors.Is(err, persistErr) {
		t.Fatalf("switch error = %v, want persistence error", err)
	}
	if !newSession.closed {
		t.Fatal("new session was not closed after persistence failure")
	}
	if !newStorage.closed {
		t.Fatal("new storage was not closed after persistence failure")
	}
	if oldSession.closed {
		t.Fatal("old session was closed before accepted switch")
	}
	if oldSession.cancels != 0 {
		t.Fatalf("old session cancels = %d, want 0 after persistence failure", oldSession.cancels)
	}
	if oldStorage.closed {
		t.Fatal("old storage was closed before accepted switch")
	}
}

func TestResumeClosesNewHandlesWhenTranscriptLoadFailsBeforePersist(t *testing.T) {
	replayErr := errors.New("bad transcript")
	oldSession := &fakeSession{id: "old"}
	oldStorage := &fakeStorage{id: "old"}
	newSession := &fakeSession{id: "resumed"}
	newStorage := &fakeStorage{id: "resumed", entriesErr: replayErr}
	saveCalled := false

	_, err := core.Resume(t.Context(), core.ResumeInput{
		Switcher: func(
			context.Context,
			*config.Config,
			string,
		) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return fakeBackend{session: newSession}, newSession, newStorage, nil
		},
		Transition: core.NewTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			core.PresetPrimary,
			"",
		).WithActivePresetPersistence(),
		Current:   core.Handles{Session: oldSession, Storage: oldStorage},
		SessionID: "resumed",
		SaveState: func(config.RuntimeStateUpdate) error {
			saveCalled = true
			return nil
		},
	})
	if !errors.Is(err, replayErr) {
		t.Fatalf("resume error = %v, want replay error", err)
	}
	if saveCalled {
		t.Fatal("resume persisted runtime state before transcript load succeeded")
	}
	if !newSession.closed {
		t.Fatal("new session was not closed after transcript load failure")
	}
	if !newStorage.closed {
		t.Fatal("new storage was not closed after transcript load failure")
	}
	if oldSession.cancels != 0 {
		t.Fatalf("old session cancels = %d, want 0 after failed resume", oldSession.cancels)
	}
	if oldSession.closed {
		t.Fatal("old session was closed after failed resume")
	}
	if oldStorage.closed {
		t.Fatal("old storage was closed after failed resume")
	}
}

type fakeBackend struct {
	provider string
	model    string
	status   string
	session  session.AgentSession
}

func (b fakeBackend) Name() string { return "fake" }

func (b fakeBackend) Provider() string { return b.provider }

func (b fakeBackend) Model() string { return b.model }

func (b fakeBackend) ContextLimit() int { return 0 }

func (b fakeBackend) Bootstrap() core.Bootstrap {
	return core.Bootstrap{Status: b.status}
}

func (b fakeBackend) Session() session.AgentSession { return b.session }

func (b fakeBackend) SetStore(session.SessionStore) {}

func (b fakeBackend) SetSession(session.SessionHandle) {}

func (b fakeBackend) SetConfig(*config.Config) {}

type fakeSession struct {
	id      string
	cancels int
	closed  bool
	events  chan session.AgentEvent
}

func (s *fakeSession) Open(context.Context) error { return nil }

func (s *fakeSession) Resume(context.Context, string) error { return nil }

func (s *fakeSession) SubmitTurn(context.Context, string) error { return nil }

func (s *fakeSession) CancelTurn(context.Context) error {
	s.cancels++
	return nil
}

func (s *fakeSession) Close() error {
	s.closed = true
	return nil
}

func (s *fakeSession) Events() <-chan session.AgentEvent { return s.events }

func (s *fakeSession) ID() string { return s.id }

func (s *fakeSession) Meta() map[string]string { return nil }

type fakeStorage struct {
	id         string
	branch     string
	closed     bool
	entries    []session.Entry
	entriesErr error
}

func (s *fakeStorage) ID() string { return s.id }

func (s *fakeStorage) Meta() session.Metadata {
	return session.Metadata{ID: s.id, Branch: s.branch}
}

func (s *fakeStorage) Append(context.Context, session.StoreEvent) error { return nil }

func (s *fakeStorage) AppendModelMessage(context.Context, llm.Message) error { return nil }

func (s *fakeStorage) ModelMessages(context.Context) ([]llm.Message, error) { return nil, nil }

func (s *fakeStorage) Entries(context.Context) ([]session.Entry, error) {
	if s.entriesErr != nil {
		return nil, s.entriesErr
	}
	return append([]session.Entry(nil), s.entries...), nil
}

func (s *fakeStorage) LastStatus(context.Context) (string, error) { return "", nil }

func (s *fakeStorage) Usage(context.Context) (int, int, float64, error) {
	return 0, 0, 0, nil
}

func (s *fakeStorage) Close() error {
	s.closed = true
	return nil
}
