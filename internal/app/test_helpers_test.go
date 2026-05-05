package app

import (
	"context"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type stubBackend struct {
	sess         *stubSession
	provider     string
	model        string
	providerSet  bool
	modelSet     bool
	contextLimit int
	surface      backend.ToolSurface
}

type compactBackend struct {
	stubBackend
	compacted bool
	err       error
	called    bool
}

type configCaptureBackend struct {
	stubBackend
	cfg *config.Config
}

func (b stubBackend) Name() string { return "stub" }
func (b stubBackend) Provider() string {
	if b.providerSet || b.provider != "" {
		return b.provider
	}
	return "stub"
}

func (b stubBackend) Model() string {
	if b.modelSet || b.model != "" {
		return b.model
	}
	return "stub-model"
}

func (b stubBackend) ContextLimit() int {
	if b.contextLimit != 0 {
		return b.contextLimit
	}
	return 0
}

func (b stubBackend) ToolSurface() backend.ToolSurface {
	if b.surface.Count != 0 ||
		b.surface.Sandbox != "" ||
		b.surface.Environment != "" ||
		len(b.surface.Names) > 0 {
		return b.surface
	}
	return backend.ToolSurface{
		Count:         2,
		LazyThreshold: 20,
		Names:         []string{"read", "write"},
	}
}

func (b stubBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{{Role: session.System, Content: "boot"}},
		Status:  "ready",
	}
}

func (b stubBackend) Session() session.AgentSession { return b.sess }

func (b stubBackend) SetStore(s storage.Store) {}

func (b stubBackend) SetSession(s storage.Session) {}

func (b stubBackend) SetConfig(cfg *config.Config) {}

func (b *configCaptureBackend) SetConfig(cfg *config.Config) {
	if cfg == nil {
		b.cfg = nil
		return
	}
	copied := *cfg
	b.cfg = &copied
}

func (b *compactBackend) Compact(ctx context.Context) (bool, error) {
	b.called = true
	return b.compacted, b.err
}

type stubSession struct {
	events      chan session.Event
	submits     []string
	cancels     int
	submitErr   error
	approveErr  error
	approvals   []stubApproval
	allowed     []string
	mode        session.Mode
	autoApprove bool
	closed      bool
}

type steeringStubSession struct {
	stubSession
	steers []string
	result session.SteeringResult
	err    error
}

type stubApproval struct {
	id string
	ok bool
}

func localErrorFromMsg(t *testing.T, msg tea.Msg) error {
	t.Helper()
	errMsg, ok := msg.(localErrorMsg)
	if !ok {
		t.Fatalf("message = %T, want localErrorMsg", msg)
	}
	return errMsg.err
}

func (s *stubSession) Open(ctx context.Context) error              { return nil }
func (s *stubSession) Resume(ctx context.Context, id string) error { return nil }
func (s *stubSession) SubmitTurn(ctx context.Context, turn string) error {
	if s.submitErr != nil {
		return s.submitErr
	}
	s.submits = append(s.submits, turn)
	return nil
}

func (s *stubSession) CancelTurn(ctx context.Context) error {
	s.cancels++
	return nil
}

func (s *stubSession) Close() error {
	s.closed = true
	if s.events != nil {
		close(s.events)
		s.events = nil
	}
	return nil
}
func (s *stubSession) Events() <-chan session.Event { return s.events }
func (s *stubSession) Approve(ctx context.Context, id string, ok bool) error {
	s.approvals = append(s.approvals, stubApproval{id: id, ok: ok})
	return s.approveErr
}

func (s *stubSession) SetMode(mode session.Mode) { s.mode = mode }

func (s *stubSession) SetAutoApprove(enabled bool) { s.autoApprove = enabled }
func (s *stubSession) AllowCategory(category string) {
	s.allowed = append(s.allowed, category)
}
func (s *stubSession) ID() string              { return "stub" }
func (s *stubSession) Meta() map[string]string { return nil }

func (s *steeringStubSession) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	s.steers = append(s.steers, text)
	if s.err != nil {
		return session.SteeringResult{}, s.err
	}
	if s.result.Outcome == "" {
		return session.SteeringResult{Outcome: session.SteeringAccepted}, nil
	}
	return s.result, nil
}

type stubStorageSession struct {
	id         string
	model      string
	branch     string
	closed     bool
	appends    []any
	appendErr  error
	usageIn    int
	usageOut   int
	usageCost  float64
	entries    []session.Entry
	entriesErr error
}

func (s *stubStorageSession) ID() string { return s.id }

func (s *stubStorageSession) Meta() storage.Metadata {
	return storage.Metadata{
		ID:     s.id,
		Model:  s.model,
		Branch: s.branch,
	}
}

func (s *stubStorageSession) Append(ctx context.Context, event any) error {
	s.appends = append(s.appends, event)
	return s.appendErr
}

func (s *stubStorageSession) Entries(ctx context.Context) ([]session.Entry, error) {
	if s.entriesErr != nil {
		return nil, s.entriesErr
	}
	return append([]session.Entry(nil), s.entries...), nil
}

func (s *stubStorageSession) LastStatus(ctx context.Context) (string, error) { return "", nil }

func (s *stubStorageSession) Usage(ctx context.Context) (int, int, float64, error) {
	return s.usageIn, s.usageOut, s.usageCost, nil
}

func (s *stubStorageSession) Close() error {
	s.closed = true
	return nil
}

type resumeOnlyStore struct {
	resumed storage.Session
}

func (s *resumeOnlyStore) OpenSession(
	ctx context.Context,
	cwd, model, branch string,
) (storage.Session, error) {
	return nil, nil
}

func (s *resumeOnlyStore) ResumeSession(ctx context.Context, id string) (storage.Session, error) {
	return s.resumed, nil
}

func (s *resumeOnlyStore) ListSessions(
	ctx context.Context,
	cwd string,
) ([]storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) GetRecentSession(
	ctx context.Context,
	cwd string,
) (*storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) AddInput(ctx context.Context, cwd, content string) error { return nil }

func (s *resumeOnlyStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	return nil, nil
}

func (s *resumeOnlyStore) UpdateSession(ctx context.Context, si storage.SessionInfo) error {
	return nil
}

func (s *resumeOnlyStore) Close() error { return nil }

type forkTreeStore struct {
	resumeOnlyStore
	forked     storage.Session
	forkParent string
	forkOpts   storage.ForkOptions
	tree       storage.SessionTree
}

func (s *forkTreeStore) ForkSession(
	ctx context.Context,
	parentID string,
	opts storage.ForkOptions,
) (storage.Session, error) {
	s.forkParent = parentID
	s.forkOpts = opts
	return s.forked, nil
}

func (s *forkTreeStore) SessionTree(
	ctx context.Context,
	sessionID string,
) (storage.SessionTree, error) {
	s.tree.Current.ID = sessionID
	return s.tree, nil
}

func readyModel(t *testing.T) Model {
	t.Helper()
	sess := &stubSession{events: make(chan session.Event)}
	b := stubBackend{sess: sess}
	model := New(b, nil, nil, "/tmp/test", "main", "dev", nil)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	ready, ok := updated.(Model)
	if !ok {
		t.Fatalf("expected Model after window size update")
	}
	return ready
}
