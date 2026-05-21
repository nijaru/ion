package canto

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/oklog/ulid/v2"
)

type eventTypeFailingCantoStore struct {
	inner    csession.Store
	failType csession.EventType
	err      error
}

func (s *eventTypeFailingCantoStore) Save(ctx context.Context, ev csession.Event) error {
	if ev.Type == s.failType {
		return s.err
	}
	return s.inner.Save(ctx, ev)
}

func (s *eventTypeFailingCantoStore) Load(
	ctx context.Context,
	sessionID string,
) (*csession.Session, error) {
	sess, err := s.inner.Load(ctx, sessionID)
	if err != nil {
		return nil, err
	}
	return sess.WithWriter(s), nil
}

func (s *eventTypeFailingCantoStore) LoadUntil(
	ctx context.Context,
	sessionID string,
	eventID ulid.ULID,
) (*csession.Session, error) {
	sess, err := s.inner.LoadUntil(ctx, sessionID, eventID)
	if err != nil {
		return nil, err
	}
	return sess.WithWriter(s), nil
}

func (s *eventTypeFailingCantoStore) Fork(
	ctx context.Context,
	originalSessionID string,
	newSessionID string,
) (*csession.Session, error) {
	sess, err := s.inner.Fork(ctx, originalSessionID, newSessionID)
	if err != nil {
		return nil, err
	}
	return sess.WithWriter(s), nil
}

type failingMetadataStore struct {
	err error
}

func (s failingMetadataStore) OpenSession(
	context.Context,
	string,
	string,
	string,
) (storage.Session, error) {
	return nil, errors.New("unexpected OpenSession")
}

func (s failingMetadataStore) ResumeSession(context.Context, string) (storage.Session, error) {
	return nil, errors.New("unexpected ResumeSession")
}

func (s failingMetadataStore) ListSessions(context.Context, string) ([]storage.SessionInfo, error) {
	return nil, errors.New("unexpected ListSessions")
}

func (s failingMetadataStore) GetRecentSession(
	context.Context,
	string,
) (*storage.SessionInfo, error) {
	return nil, errors.New("unexpected GetRecentSession")
}

func (s failingMetadataStore) AddInput(context.Context, string, string) error {
	return errors.New("unexpected AddInput")
}

func (s failingMetadataStore) GetInputs(context.Context, string, int) ([]string, error) {
	return nil, errors.New("unexpected GetInputs")
}

func (s failingMetadataStore) UpdateSession(context.Context, storage.SessionInfo) error {
	return s.err
}

func (s failingMetadataStore) Close() error {
	return nil
}

type blockingMetadataStore struct {
	failingMetadataStore
	started chan struct{}
}

func (s blockingMetadataStore) UpdateSession(ctx context.Context, _ storage.SessionInfo) error {
	close(s.started)
	<-ctx.Done()
	return ctx.Err()
}

type recordingMetadataStore struct {
	failingMetadataStore
	updates int
}

func (s *recordingMetadataStore) UpdateSession(context.Context, storage.SessionInfo) error {
	s.updates++
	return nil
}

type staticStorageSession struct {
	id   string
	meta storage.Metadata
}

func (s staticStorageSession) ID() string { return s.id }

func (s staticStorageSession) Meta() storage.Metadata { return s.meta }

func (s staticStorageSession) Append(context.Context, any) error {
	return errors.New("unexpected Append")
}

func (s staticStorageSession) Entries(context.Context) ([]ionsession.Entry, error) {
	return nil, errors.New("unexpected Entries")
}

func (s staticStorageSession) LastStatus(context.Context) (string, error) {
	return "", errors.New("unexpected LastStatus")
}

func (s staticStorageSession) Usage(context.Context) (int, int, float64, error) {
	return 0, 0, 0, errors.New("unexpected Usage")
}

func (s staticStorageSession) Close() error {
	return nil
}

type blockingLazySession struct {
	staticStorageSession
	started chan struct{}
	release chan struct{}
}

func (s blockingLazySession) Ensure(context.Context) (storage.Session, error) {
	close(s.started)
	<-s.release
	return s.staticStorageSession, nil
}

func TestSubmitTurnMaterializesLazySession(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := "/tmp/ion-lazy-turn"
	storageSession := storage.NewLazySession(store, cwd, "local-api/model-a", "main")

	provider := llm.NewFauxProvider("local-api", llm.FauxStep{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if storage.IsMaterialized(storageSession) {
		t.Fatal("lazy session materialized during backend open")
	}
	before, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list before submit: %v", err)
	}
	if len(before) != 0 {
		t.Fatalf("sessions before submit = %#v, want none", before)
	}

	if err := b.Session().SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	if !storage.IsMaterialized(storageSession) {
		t.Fatal("lazy session not materialized by submit")
	}
	after, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list after submit: %v", err)
	}
	if len(after) != 1 {
		t.Fatalf("sessions after submit = %d, want 1", len(after))
	}
	if after[0].LastPreview != "hi" {
		t.Fatalf("last preview = %q, want hi", after[0].LastPreview)
	}
}

func TestSubmitTurnMetadataUpdateFailureDoesNotLeaveActiveTurn(t *testing.T) {
	updateErr := errors.New("metadata update failed")
	b := New()
	b.harness = &cantofw.Harness{}
	b.SetStore(failingMetadataStore{err: updateErr})
	b.SetSession(staticStorageSession{
		id: "session-id",
		meta: storage.Metadata{
			ID:     "session-id",
			CWD:    "/tmp/ion-test",
			Model:  "openai/model-a",
			Branch: "main",
		},
	})
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})

	err := b.Session().SubmitTurn(t.Context(), "hi")
	if !errors.Is(err, updateErr) {
		t.Fatalf("SubmitTurn error = %v, want metadata update failure", err)
	}
	if b.turn.active {
		t.Fatal("metadata update failure left a turn active")
	}
	assertNoBackendEvent(t, b)
}

func TestCancelTurnDuringMetadataUpdateDoesNotWaitForStore(t *testing.T) {
	b := New()
	b.harness = &cantofw.Harness{}
	started := make(chan struct{})
	b.SetStore(blockingMetadataStore{started: started})
	b.SetSession(staticStorageSession{
		id: "session-id",
		meta: storage.Metadata{
			ID:     "session-id",
			CWD:    "/tmp/ion-test",
			Model:  "openai/model-a",
			Branch: "main",
		},
	})
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})

	done := make(chan error, 1)
	go func() {
		done <- b.Session().SubmitTurn(t.Context(), "hi")
	}()

	select {
	case <-started:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for metadata update")
	}

	cancelDone := make(chan error, 1)
	go func() {
		cancelDone <- b.Session().CancelTurn(t.Context())
	}()

	select {
	case err := <-cancelDone:
		if err != nil {
			t.Fatalf("cancel turn: %v", err)
		}
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("CancelTurn waited for metadata update")
	}

	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("SubmitTurn error = %v, want context canceled", err)
		}
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for SubmitTurn to exit after cancel")
	}
	if b.turn.active {
		t.Fatal("canceled metadata update left a turn active")
	}
	assertNoBackendEvent(t, b)
}

func TestCancelTurnDuringLazySessionOpenSkipsMetadataUpdate(t *testing.T) {
	b := New()
	b.harness = &cantofw.Harness{}
	started := make(chan struct{})
	release := make(chan struct{})
	metadata := &recordingMetadataStore{}
	b.SetStore(metadata)
	b.SetSession(blockingLazySession{
		staticStorageSession: staticStorageSession{
			id: "session-id",
			meta: storage.Metadata{
				ID:     "session-id",
				CWD:    "/tmp/ion-test",
				Model:  "openai/model-a",
				Branch: "main",
			},
		},
		started: started,
		release: release,
	})
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})

	done := make(chan error, 1)
	go func() {
		done <- b.Session().SubmitTurn(t.Context(), "hi")
	}()

	select {
	case <-started:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for lazy session open")
	}
	if err := b.Session().CancelTurn(t.Context()); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}
	close(release)

	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("SubmitTurn error = %v, want context canceled", err)
		}
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for SubmitTurn to exit after cancel")
	}
	if metadata.updates != 0 {
		t.Fatalf("metadata updates = %d, want none after cancel", metadata.updates)
	}
	if b.turn.active {
		t.Fatal("canceled lazy open left a turn active")
	}
	assertNoBackendEvent(t, b)
}

func TestSubmitTurnDefaultsToTrustedWriteToolAndPersistsFile(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	call := llm.Call{ID: "write-call-1", Type: "function"}
	call.Function.Name = "write"
	call.Function.Arguments = `{"file_path":"handoff.md","content":"ion smoke ok\n"}`
	provider := ctesting.NewFauxProvider(
		"local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "done"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "write the smoke file"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	events := b.Session().Events()
	var (
		seenWriteStart  bool
		seenWriteResult bool
		assistant       string
	)
	timeout := time.After(backendEventWaitTimeout)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before write turn finished")
			}
			switch msg := ev.(type) {
			case ionsession.ToolCallStarted:
				if msg.ToolName == "write" && strings.Contains(msg.Args, "handoff.md") {
					seenWriteStart = true
				}
			case ionsession.ToolResult:
				if msg.ToolName == "write" && strings.Contains(msg.Result, "Wrote handoff.md.") {
					seenWriteResult = true
				}
			case ionsession.AgentMessage:
				assistant = msg.Message
			case ionsession.Error:
				t.Fatalf("unexpected session error: %v", msg.Err)
			case ionsession.TurnFinished:
				if !seenWriteStart {
					t.Fatal("missing write tool start event")
				}
				if !seenWriteResult {
					t.Fatal("missing write tool result event")
				}
				if !strings.Contains(assistant, "done") {
					t.Fatalf("assistant = %q, want final done response", assistant)
				}
				got, err := os.ReadFile(filepath.Join(cwd, "handoff.md"))
				if err != nil {
					t.Fatalf("read written file: %v", err)
				}
				if string(got) != "ion smoke ok\n" {
					t.Fatalf("written file = %q, want smoke content", got)
				}
				calls := provider.Calls()
				if len(calls) != 2 {
					t.Fatalf("provider calls = %d, want initial and post-tool requests", len(calls))
				}
				if !requestHasMessage(calls[1].Messages, llm.RoleTool, "Wrote handoff.md.") {
					t.Fatal("post-tool request missing write result")
				}
				entries, err := storageSession.Entries(ctx)
				if err != nil {
					t.Fatalf("entries: %v", err)
				}
				if !entryExists(entries, ionsession.Tool, "Wrote handoff.md.") {
					t.Fatal("persisted entries missing write tool result")
				}
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for write turn")
		}
	}
}

func TestSubmitTurnEmptyAssistantResponseEmitsSessionError(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := ctesting.NewFauxProvider("local-api", ctesting.Step{})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "return nothing"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	var sawError bool
	timeout := time.After(backendEventWaitTimeout)
	for {
		select {
		case ev, ok := <-b.Session().Events():
			if !ok {
				t.Fatal("event stream closed before empty-response turn finished")
			}
			switch msg := ev.(type) {
			case ionsession.AgentMessage:
				t.Fatalf("unexpected assistant message for empty response: %#v", msg)
			case ionsession.Error:
				if msg.Err == nil ||
					!strings.Contains(msg.Err.Error(), "assistant response has no content") {
					t.Fatalf("session error = %v, want empty assistant response error", msg.Err)
				}
				sawError = true
			case ionsession.TurnFinished:
				if !sawError {
					t.Fatal("turn finished before empty-response error")
				}
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for empty-response turn")
		}
	}
}

func TestSubmitTurnBashEmitsToolOutputDeltas(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	call := llm.Call{ID: "bash-call-1", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"printf ion-stream-output"}`
	provider := ctesting.NewFauxProvider(
		"local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "done"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "run the streaming command"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	events := b.Session().Events()
	var (
		deltas []string
		result string
	)
	timeout := time.After(backendEventWaitTimeout)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before bash turn finished")
			}
			switch msg := ev.(type) {
			case ionsession.ToolOutputDelta:
				if msg.ToolUseID == "bash-call-1" {
					deltas = append(deltas, msg.Delta)
				}
			case ionsession.ToolResult:
				if msg.ToolUseID == "bash-call-1" {
					result = msg.Result
				}
			case ionsession.Error:
				t.Fatalf("unexpected session error: %v", msg.Err)
			case ionsession.TurnFinished:
				combined := strings.Join(deltas, "")
				if !strings.Contains(combined, "ion-stream-output") {
					t.Fatalf("tool output deltas = %q, want streamed bash output", combined)
				}
				if result != "ion-stream-output" {
					t.Fatalf("tool result = %q, want final output once", result)
				}
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for bash streaming turn")
		}
	}
}

func TestSubmitTurnStreamingDeltaPersistenceErrorFinishesTurn(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	innerStore, err := csession.NewSQLiteStore(filepath.Join(t.TempDir(), "canto.sqlite"))
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	persistErr := errors.New("persist tool delta")
	failingStore := &eventTypeFailingCantoStore{
		inner:    innerStore,
		failType: csession.ToolOutputDelta,
		err:      persistErr,
	}
	cwd := t.TempDir()
	ionStore, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new ion store: %v", err)
	}
	storageSession, err := ionStore.OpenSession(
		ctx,
		cwd,
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open ion session: %v", err)
	}

	call := llm.Call{ID: "bash-call-persist-error", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"printf ion-stream-output"}`
	provider := ctesting.NewFauxProvider(
		"local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "should-not-run"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.store = failingStore
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "run the streaming command"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	events := b.Session().Events()
	var sawError bool
	timeout := time.After(backendEventWaitTimeout)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before bash turn finished")
			}
			switch msg := ev.(type) {
			case ionsession.Error:
				sawError = true
				if !strings.Contains(msg.Err.Error(), persistErr.Error()) {
					t.Fatalf("session error = %v, want %v", msg.Err, persistErr)
				}
			case ionsession.ToolOutputDelta:
				t.Fatalf("unexpected persisted tool output delta: %#v", msg)
			case ionsession.ToolResult:
				t.Fatalf("unexpected tool result after delta persistence error: %#v", msg)
			case ionsession.TurnFinished:
				if !sawError {
					t.Fatal("turn finished without surfacing delta persistence error")
				}
				if b.turn.active {
					t.Fatal("turn remained active after delta persistence error")
				}
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for bash persistence error turn")
		}
	}
}

func TestSubmitTurnBashTruncatesStreamedToolResult(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	call := llm.Call{ID: "bash-call-large", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = fmt.Sprintf(
		`{"command":"awk 'BEGIN { for (i = 0; i < %d; i++) printf \"a\" }'"}`,
		1024*1024+64,
	)
	provider := ctesting.NewFauxProvider(
		"local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "done"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "run the large streaming command"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	events := b.Session().Events()
	var (
		deltaBytes int
		resultErr  error
		result     string
	)
	timeout := time.After(backendEventWaitTimeout)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before bash turn finished")
			}
			switch msg := ev.(type) {
			case ionsession.ToolOutputDelta:
				if msg.ToolUseID == "bash-call-large" {
					deltaBytes += len(msg.Delta)
				}
			case ionsession.ToolResult:
				if msg.ToolUseID == "bash-call-large" {
					resultErr = msg.Error
					result = msg.Result
				}
			case ionsession.Error:
				t.Fatalf("unexpected session error: %v", msg.Err)
			case ionsession.TurnFinished:
				if !strings.Contains(result, "[tool output truncated after") {
					rawLen, rawHasMarker := rawToolCompletedOutputInfo(t, b, "bash-call-large")
					t.Fatalf(
						"tool result missing truncation marker; result len=%d delta bytes=%d err=%v raw len=%d raw marker=%v suffix=%q",
						len(result),
						deltaBytes,
						resultErr,
						rawLen,
						rawHasMarker,
						tailString(result, 200),
					)
				}
				if !strings.Contains(result, "64 bytes omitted") {
					t.Fatalf(
						"tool result = %q, want omitted byte count",
						tailString(result, 200),
					)
				}
				if len(result) > 1024*1024+512 {
					t.Fatalf("tool result length = %d, want bounded output", len(result))
				}
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for large bash streaming turn")
		}
	}
}

func TestSubmitTurnUsesCallerContext(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, "/tmp/ion-context", "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	turnCtx, cancel := context.WithCancel(ctx)
	if err := b.Session().SubmitTurn(turnCtx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	var streamCtx context.Context
	select {
	case streamCtx = <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}

	cancel()
	select {
	case <-streamCtx.Done():
	case <-time.After(2 * time.Second):
		t.Fatal("provider stream context was not canceled")
	}
	waitForTurnFinished(t, b.Session().Events())
}

func TestSubmitTurnCancelSuppressesLateAssistant(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-late-cancel",
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &lateSuccessStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	select {
	case <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}
	if err := b.Session().CancelTurn(ctx); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}

	for {
		select {
		case ev := <-b.Session().Events():
			switch msg := ev.(type) {
			case ionsession.AgentMessage:
				t.Fatalf("late assistant reached Ion after cancel: %#v", msg)
			case ionsession.TurnFinished:
				entries, err := storageSession.Entries(ctx)
				if err != nil {
					t.Fatalf("load entries: %v", err)
				}
				for _, entry := range entries {
					if entry.Role == ionsession.Agent {
						t.Fatalf("late assistant persisted after cancel: %#v", entry)
					}
				}
				return
			}
		case <-time.After(2 * time.Second):
			t.Fatal("timed out waiting for canceled turn")
		}
	}
}

func TestSubmitTurnCancelDuringToolSuppressesLateToolEvents(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		t.TempDir(),
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	call := llm.Call{ID: "tool-call-cancel", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"sleep 10; echo late-tool-output"}`
	provider := ctesting.NewFauxProvider(
		"local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "late assistant after canceled tool"},
		ctesting.Step{Content: "recovered after canceled tool"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "run a long command"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	events := b.Session().Events()
	seenTool := false
	for !seenTool {
		select {
		case ev := <-events:
			switch ev.(type) {
			case ionsession.ToolCallStarted:
				seenTool = true
			case ionsession.Error:
				t.Fatalf("unexpected error before cancel: %#v", ev)
			}
		case <-time.After(2 * time.Second):
			t.Fatal("timed out waiting for tool call")
		}
	}

	if err := b.Session().CancelTurn(ctx); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}
	waitForTurnFinishedAfterError(t, events)

	quiet := time.NewTimer(300 * time.Millisecond)
	defer quiet.Stop()
	for {
		select {
		case ev := <-events:
			switch msg := ev.(type) {
			case ionsession.ToolResult:
				t.Fatalf("late tool result reached Ion after cancel: %#v", msg)
			case ionsession.AgentMessage:
				t.Fatalf("late assistant reached Ion after cancel: %#v", msg)
			case ionsession.Error:
				t.Fatalf("late error reached Ion after cancel: %v", msg.Err)
			case ionsession.TurnFinished:
				t.Fatalf("duplicate turn finished after cancel: %#v", msg)
			}
		case <-quiet.C:
			if calls := provider.Calls(); len(calls) != 1 {
				t.Fatalf(
					"provider calls after canceled tool = %d, want initial request only",
					len(calls),
				)
			}
			entries, err := storageSession.Entries(ctx)
			if err != nil {
				t.Fatalf("entries: %v", err)
			}
			for _, entry := range entries {
				if entry.Role == ionsession.Tool &&
					strings.Contains(entry.Content, "late-tool-output") {
					t.Fatalf("late tool output persisted after cancel: %#v", entry)
				}
				if entry.Role == ionsession.Agent &&
					strings.Contains(entry.Content, "late assistant after canceled tool") {
					t.Fatalf("late assistant persisted after cancel: %#v", entry)
				}
			}
			return
		}
	}
}

func TestSubmitTurnRejectsConcurrentTurn(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-concurrent",
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	turnCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	if err := b.Session().SubmitTurn(turnCtx, "first"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}

	select {
	case <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}

	err = b.Session().SubmitTurn(ctx, "second")
	if err == nil || !strings.Contains(err.Error(), "turn already in progress") {
		t.Fatalf("second SubmitTurn error = %v, want turn already in progress", err)
	}

	cancel()
	waitForTurnFinished(t, b.Session().Events())
}

func TestSubmitTurnCancelDuringProactiveCompactionSuppressesError(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-cancel-before-compact",
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	provider := &blockingCountProvider{
		compactProvider: compactProvider{id: "local-api"},
		entered:         make(chan struct{}),
	}
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider:     "local-api",
			Model:        "model-a",
			Endpoint:     "http://localhost:8080/v1",
			ContextLimit: 100,
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "cancel before compaction finishes"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	select {
	case <-provider.entered:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for proactive compaction token check")
	}
	if err := b.Session().CancelTurn(ctx); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())
}

func TestResumeDoesNotDeadlockWhenBackendNeedsOpen(t *testing.T) {
	b := New()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- b.Session().Resume(ctx, "session-id")
	}()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("expected resume to fail without provider/model")
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("resume appears to deadlock")
	}
}

func TestCloseIsIdempotent(t *testing.T) {
	b := New()

	if err := b.Session().Close(); err != nil {
		t.Fatalf("first close: %v", err)
	}
	if err := b.Session().Close(); err != nil {
		t.Fatalf("second close: %v", err)
	}
}

func TestSubmitTurnToolFailurePersistsForFollowUp(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	call := llm.Call{ID: "tool-call-fail", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"exit 7"}`
	provider := ctesting.NewFauxProvider(
		"local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "handled tool failure"},
		ctesting.Step{Content: "continued"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "run a failing command"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	if err := b.Session().SubmitTurn(ctx, "can you continue after that failure?"); err != nil {
		t.Fatalf("submit follow-up turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3", len(calls))
	}
	postToolRequest := calls[1]
	if !requestHasMessage(postToolRequest.Messages, llm.RoleTool, "exit status 7") {
		t.Fatalf("post-tool request missing failed tool result: %#v", postToolRequest.Messages)
	}
	followUpRequest := calls[2]
	if !requestHasMessage(followUpRequest.Messages, llm.RoleAssistant, "handled tool failure") {
		t.Fatalf(
			"follow-up request missing post-tool assistant reply: %#v",
			followUpRequest.Messages,
		)
	}
	if !requestHasMessage(followUpRequest.Messages, llm.RoleTool, "exit status 7") {
		t.Fatalf("follow-up request missing failed tool result: %#v", followUpRequest.Messages)
	}
}

func TestSubmitTurnProviderErrorLeavesBackendReusable(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-provider-error",
		"openai/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	providerErr := errors.New("provider unavailable")
	provider := ctesting.NewFauxProvider(
		"openai",
		ctesting.Step{Err: providerErr},
		ctesting.Step{Content: "recovered reply"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "first turn fails"); err != nil {
		t.Fatalf("submit failing turn: %v", err)
	}
	errEvent := waitForSessionError(t, b.Session().Events())
	if !strings.Contains(errEvent.Err.Error(), providerErr.Error()) {
		t.Fatalf("error = %v, want provider error", errEvent.Err)
	}
	waitForTurnFinished(t, b.Session().Events())

	if err := b.Session().SubmitTurn(ctx, "second turn recovers"); err != nil {
		t.Fatalf("submit recovery turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2", len(calls))
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	cantoSess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var terminalErrorFound bool
	for _, ev := range cantoSess.Events() {
		if ev.Type != csession.TurnCompleted {
			continue
		}
		data, ok, err := ev.TurnCompletedData()
		if err != nil {
			t.Fatalf("decode turn completed: %v", err)
		}
		if ok && strings.Contains(data.Error, providerErr.Error()) {
			terminalErrorFound = true
		}
	}
	if !terminalErrorFound {
		t.Fatalf("missing durable provider error terminal event")
	}
}

func TestRunTurnDoesNotSynthesizeTerminalEventAfterCantoSettlement(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	events := make(chan cantofw.RunEvent)
	close(events)

	b.runTurn(
		t.Context(),
		turnID,
		"hi",
		func() {},
		turnSubmitFunc(func(context.Context, string) (cantoTurnHandle, error) {
			return &fakeCantoTurn{events: events}, nil
		}),
	)

	assertNoBackendEvent(t, b)
	if b.turn.active {
		t.Fatal("turn remained active after Canto settlement")
	}
}

func TestRunTurnReportsCantoResultErrorWithoutLifecycle(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	events := make(chan cantofw.RunEvent)
	close(events)
	resultErr := errors.New("provider failed")

	b.runTurn(
		t.Context(),
		turnID,
		"hi",
		func() {},
		turnSubmitFunc(func(context.Context, string) (cantoTurnHandle, error) {
			return &fakeCantoTurn{events: events, resultErr: resultErr}, nil
		}),
	)

	errEvent := waitForSessionError(t, b.Session().Events())
	if !errors.Is(errEvent.Err, resultErr) {
		t.Fatalf("error = %v, want result error", errEvent.Err)
	}
	waitForTurnFinished(t, b.Session().Events())
	assertNoBackendEvent(t, b)
}

func TestRunTurnTreatsCancellationRunEventAsQuietTerminal(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	events := make(chan cantofw.RunEvent, 1)
	events <- cantofw.RunEvent{Type: cantofw.RunEventError, Err: context.Canceled}
	close(events)

	b.runTurn(
		t.Context(),
		turnID,
		"hi",
		func() {},
		turnSubmitFunc(func(context.Context, string) (cantoTurnHandle, error) {
			return &fakeCantoTurn{events: events}, nil
		}),
	)

	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("cancellation stream error did not emit TurnFinished")
	}
	assertNoBackendEvent(t, b)
}

func TestCancelTurnWaitsForStreamSettlement(t *testing.T) {
	b := New()
	ctx, cancel := context.WithCancel(t.Context())
	turnID := b.turn.start(cancel)
	ready := make(chan struct{})
	release := make(chan struct{})
	events := make(chan cantofw.RunEvent)

	done := make(chan struct{})
	go func() {
		defer close(done)
		b.runTurn(
			ctx,
			turnID,
			"hi",
			cancel,
			turnSubmitFunc(func(ctx context.Context, _ string) (cantoTurnHandle, error) {
				close(ready)
				go func() {
					defer close(events)
					<-ctx.Done()
					<-release
					events <- cantofw.RunEvent{
						Type: cantofw.RunEventSession,
						Event: csession.NewTurnCompletedEvent(
							"session-id",
							csession.TurnCompletedData{Error: context.Canceled.Error()},
						),
					}
					events <- cantofw.RunEvent{Type: cantofw.RunEventError, Err: context.Canceled}
				}()
				return &fakeCantoTurn{events: events}, nil
			}),
		)
	}()

	select {
	case <-ready:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for prompt stream")
	}
	if err := b.Session().CancelTurn(t.Context()); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}
	assertNoBackendEvent(t, b)

	close(release)
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("canceled stream settlement did not emit TurnFinished")
	}
	assertNoBackendEvent(t, b)
	select {
	case <-done:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for runTurn to exit")
	}
}

func TestCanceledStreamSettlementDoesNotFinishNextTurn(t *testing.T) {
	b := New()
	ctx, cancel := context.WithCancel(t.Context())
	oldTurnID := b.turn.start(cancel)
	ready := make(chan struct{})
	release := make(chan struct{})
	events := make(chan cantofw.RunEvent)

	done := make(chan struct{})
	go func() {
		defer close(done)
		b.runTurn(
			ctx,
			oldTurnID,
			"old",
			cancel,
			turnSubmitFunc(func(ctx context.Context, _ string) (cantoTurnHandle, error) {
				close(ready)
				go func() {
					defer close(events)
					<-ctx.Done()
					<-release
					events <- cantofw.RunEvent{
						Type: cantofw.RunEventSession,
						Event: csession.NewTurnCompletedEvent(
							"session-id",
							csession.TurnCompletedData{Error: context.Canceled.Error()},
						),
					}
					events <- cantofw.RunEvent{Type: cantofw.RunEventError, Err: context.Canceled}
				}()
				return &fakeCantoTurn{events: events}, nil
			}),
		)
	}()

	select {
	case <-ready:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for prompt stream")
	}
	if err := b.Session().CancelTurn(t.Context()); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}
	nextTurnID := b.turn.start(func() {})

	close(release)
	select {
	case <-done:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for old runTurn to exit")
	}
	assertNoBackendEvent(t, b)
	if !b.turn.activeFor(nextTurnID) {
		t.Fatal("old canceled settlement finished the next turn")
	}
}

type turnSubmitFunc func(context.Context, string) (cantoTurnHandle, error)

func (f turnSubmitFunc) submit(
	ctx context.Context,
	message string,
) (cantoTurnHandle, error) {
	return f(ctx, message)
}

type fakeCantoTurn struct {
	events    <-chan cantofw.RunEvent
	cancel    func()
	result    agent.StepResult
	resultErr error
}

func (t *fakeCantoTurn) Events() <-chan cantofw.RunEvent {
	return t.events
}

func (t *fakeCantoTurn) Cancel(ctx context.Context) error {
	if ctx != nil {
		if err := ctx.Err(); err != nil {
			return err
		}
	}
	if t.cancel != nil {
		t.cancel()
	}
	return nil
}

func (t *fakeCantoTurn) Result() (agent.StepResult, error) {
	return t.result, t.resultErr
}

func tailString(s string, max int) string {
	if len(s) <= max {
		return s
	}
	return s[len(s)-max:]
}

func rawToolCompletedOutputInfo(t *testing.T, b *Backend, toolUseID string) (int, bool) {
	t.Helper()
	if b.harness == nil || b.harness.Runner == nil {
		return 0, false
	}
	events, err := b.harness.Runner.Events(t.Context(), b.Session().ID())
	if err != nil {
		t.Fatalf("raw events: %v", err)
	}
	for _, ev := range events {
		if ev.Type != csession.ToolCompleted {
			continue
		}
		data, ok, err := ev.ToolCompletedData()
		if err != nil {
			t.Fatalf("decode raw tool completed: %v", err)
		}
		if ok && data.ID == toolUseID {
			return len(data.Output), strings.Contains(data.Output, "[tool output truncated after")
		}
	}
	return 0, false
}
