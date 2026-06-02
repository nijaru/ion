package app

import (
	"context"
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type stubBackend struct {
	sess         session.AgentSession
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
		Entries: []session.Entry{{Role: session.RoleSystem, Content: "boot"}},
		Status:  "ready",
	}
}

func (b stubBackend) Session() session.AgentSession { return b.sess }

func (b stubBackend) SetStore(s session.SessionStore) {}

func (b stubBackend) SetSession(s session.SessionHandle) {}

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
	events    chan session.AgentEvent
	submits   []string
	cancels   int
	submitErr error
	closed    bool
}

type steeringStubSession struct {
	stubSession
	steers []string
	result session.SteeringResult
	err    error
}

type queuedInputStubSession struct {
	stubSession
	followUps []string
	clears    int
	result    session.QueuedInputResult
	err       error
	clearErr  error
}

func localErrorFromMsg(t *testing.T, msg tea.Msg) error {
	t.Helper()
	switch msg := msg.(type) {
	case localErrorMsg:
		return msg.err
	case runtimeSwitchErrorMsg:
		return msg.err
	default:
		t.Fatalf("message = %T, want localErrorMsg", msg)
		return nil
	}
}

func requireSequenceCmd(t *testing.T, cmd tea.Cmd) {
	t.Helper()
	if cmd == nil {
		t.Fatal("expected command")
	}
	if got := fmt.Sprintf("%T", cmd()); got != "tea.sequenceMsg" {
		t.Fatalf("command = %s, want tea.sequenceMsg", got)
	}
}

func requireBatchCmd(t *testing.T, cmd tea.Cmd) {
	t.Helper()
	if cmd == nil {
		t.Fatal("expected command")
	}
	if got := fmt.Sprintf("%T", cmd()); got != "tea.BatchMsg" {
		t.Fatalf("command = %s, want tea.BatchMsg", got)
	}
}

func settleRuntimeTransitionCmd(t *testing.T, model Model, cmd tea.Cmd) (Model, tea.Cmd) {
	t.Helper()
	if cmd == nil {
		t.Fatal("expected runtime transition command")
	}
	msg := cmd()
	updated, nextCmd := model.Update(msg)
	next := testModel(t, updated)
	return next, nextCmd
}

func runCommandTree(t *testing.T, cmd tea.Cmd) []tea.Msg {
	t.Helper()
	if cmd == nil {
		return nil
	}
	msg := cmd()
	if msg == nil {
		return nil
	}
	value := reflect.ValueOf(msg)
	if value.Kind() != reflect.Slice {
		return []tea.Msg{msg}
	}
	cmdType := reflect.TypeOf(tea.Cmd(nil))
	if value.Type().Elem() != cmdType {
		return []tea.Msg{msg}
	}
	var messages []tea.Msg
	for i := range value.Len() {
		child, ok := value.Index(i).Interface().(tea.Cmd)
		if !ok {
			t.Fatalf("sequence element %d = %T, want tea.Cmd", i, value.Index(i).Interface())
		}
		messages = append(messages, runCommandTree(t, child)...)
	}
	return messages
}

func commandChildren(t *testing.T, msg tea.Msg) []tea.Cmd {
	t.Helper()
	value := reflect.ValueOf(msg)
	cmdType := reflect.TypeOf(tea.Cmd(nil))
	if value.Kind() != reflect.Slice || value.Type().Elem() != cmdType {
		t.Fatalf("message = %T, want command batch/sequence", msg)
	}
	children := make([]tea.Cmd, 0, value.Len())
	for i := range value.Len() {
		child, ok := value.Index(i).Interface().(tea.Cmd)
		if !ok {
			t.Fatalf("command element %d = %T, want tea.Cmd", i, value.Index(i).Interface())
		}
		children = append(children, child)
	}
	return children
}

func runSequencePrefix(t *testing.T, cmd tea.Cmd, limit int) []tea.Msg {
	t.Helper()
	if cmd == nil || limit <= 0 {
		return nil
	}
	msg := cmd()
	if msg == nil {
		return nil
	}
	value := reflect.ValueOf(msg)
	cmdType := reflect.TypeOf(tea.Cmd(nil))
	if value.Kind() != reflect.Slice || value.Type().Elem() != cmdType {
		return []tea.Msg{msg}
	}
	var messages []tea.Msg
	for i := 0; i < value.Len() && i < limit; i++ {
		child, ok := value.Index(i).Interface().(tea.Cmd)
		if !ok {
			t.Fatalf("sequence element %d = %T, want tea.Cmd", i, value.Index(i).Interface())
		}
		messages = append(messages, runCommandTree(t, child)...)
	}
	return messages
}

func containsSessionEvent[T session.AgentEvent](messages []tea.Msg) bool {
	for _, msg := range messages {
		eventMsg, ok := msg.(sessionEventMsg)
		if !ok {
			continue
		}
		if _, ok := eventMsg.event.(T); ok {
			return true
		}
	}
	return false
}

func testModel(t testing.TB, updated any) Model {
	t.Helper()
	switch next := updated.(type) {
	case Model:
		return next
	case *Model:
		if next == nil {
			t.Fatal("updated model is nil")
		}
		return *next
	default:
		t.Fatalf("updated model = %T, want Model", updated)
		return Model{}
	}
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
func (s *stubSession) Events() <-chan session.AgentEvent { return s.events }

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

func (s *queuedInputStubSession) FollowUpTurn(
	ctx context.Context,
	text string,
) (session.QueuedInputResult, error) {
	s.followUps = append(s.followUps, text)
	if s.err != nil {
		return session.QueuedInputResult{}, s.err
	}
	if s.result.Outcome == "" {
		return session.QueuedInputResult{Outcome: session.QueuedInputAccepted}, nil
	}
	return s.result, nil
}

func (s *queuedInputStubSession) ClearQueuedInput(
	ctx context.Context,
) (session.QueuedInputSnapshot, error) {
	s.clears++
	if s.clearErr != nil {
		return session.QueuedInputSnapshot{}, s.clearErr
	}
	return session.QueuedInputSnapshot{}, nil
}

type stubStorageSession struct {
	id         string
	model      string
	branch     string
	closed     bool
	appends    []any
	messages   []llm.Message
	appendErr  error
	usageIn    int
	usageOut   int
	usageCost  float64
	entries    []session.Entry
	entriesErr error
}

func (s *stubStorageSession) ID() string { return s.id }

func (s *stubStorageSession) Meta() session.Metadata {
	return session.Metadata{
		ID:     s.id,
		Model:  s.model,
		Branch: s.branch,
	}
}

func (s *stubStorageSession) Append(ctx context.Context, event session.StoreEvent) error {
	s.appends = append(s.appends, event)
	return s.appendErr
}

func (s *stubStorageSession) AppendModelMessage(
	ctx context.Context,
	message llm.Message,
) error {
	s.messages = append(s.messages, message)
	return s.appendErr
}

func (s *stubStorageSession) ModelMessages(ctx context.Context) ([]llm.Message, error) {
	return append([]llm.Message(nil), s.messages...), nil
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
	resumed session.SessionHandle
}

func (s *resumeOnlyStore) OpenSession(
	ctx context.Context,
	cwd, model, branch string,
) (session.SessionHandle, error) {
	return nil, nil
}

func (s *resumeOnlyStore) ResumeSession(ctx context.Context, id string) (session.SessionHandle, error) {
	return s.resumed, nil
}

func (s *resumeOnlyStore) ListSessions(
	ctx context.Context,
	cwd string,
) ([]session.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) GetRecentSession(
	ctx context.Context,
	cwd string,
) (*session.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) AddInput(ctx context.Context, cwd, content string) error { return nil }

func (s *resumeOnlyStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	return nil, nil
}

func (s *resumeOnlyStore) UpdateSession(ctx context.Context, si session.SessionInfo) error {
	return nil
}

func (s *resumeOnlyStore) Close() error { return nil }

func readyModel(t *testing.T) Model {
	t.Helper()
	// Isolate from user's global config in ~/.ion/config.toml
	if home, err := os.UserHomeDir(); err == nil {
		if !strings.Contains(home, "tmp") && !strings.Contains(home, "TempDir") &&
			!strings.Contains(home, "folders") &&
			!strings.Contains(home, "/var/") {
			t.Setenv("HOME", t.TempDir())
		}
	}
	sess := &stubSession{events: make(chan session.AgentEvent)}
	b := stubBackend{sess: sess}
	model := New(b, nil, nil, "/tmp/test", "main", "dev", nil)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	return testModel(t, updated)
}

func withOpenRouterKey(t *testing.T) {
	t.Helper()
	t.Setenv("OPENROUTER_API_KEY", "test-key")
}

func resolveModelPickerLoad(t *testing.T, model Model, cmd tea.Cmd) Model {
	t.Helper()
	if cmd == nil {
		return model
	}
	msg := cmd()
	updated, nextCmd := model.Update(msg)
	if nextCmd != nil {
		t.Fatalf("model picker load returned unexpected command %T", nextCmd)
	}
	return testModel(t, updated)
}

func resolveProviderSelection(t *testing.T, model Model, cmd tea.Cmd) (Model, tea.Cmd) {
	t.Helper()
	if cmd == nil {
		return model, nil
	}
	msg := cmd()
	updated, nextCmd := model.Update(msg)
	return testModel(t, updated), nextCmd
}

func resolveProviderSelectionAndModelLoad(t *testing.T, model Model, cmd tea.Cmd) Model {
	t.Helper()
	model, nextCmd := resolveProviderSelection(t, model, cmd)
	return resolveModelPickerLoad(t, model, nextCmd)
}

func resolveModelPickerSetup(t *testing.T, model Model, cmd tea.Cmd) (Model, tea.Cmd) {
	t.Helper()
	if cmd == nil {
		return model, nil
	}
	msg := cmd()
	updated, nextCmd := model.Update(msg)
	return testModel(t, updated), nextCmd
}

func resolveSetupPromptSave(t *testing.T, model Model, cmd tea.Cmd) (Model, tea.Cmd) {
	t.Helper()
	if cmd == nil {
		return model, nil
	}
	msg := cmd()
	updated, nextCmd := model.Update(msg)
	return testModel(t, updated), nextCmd
}

func resolveSettingsCommand(t *testing.T, model Model, cmd tea.Cmd) (Model, tea.Cmd) {
	t.Helper()
	if cmd == nil {
		return model, nil
	}
	msg := cmd()
	updated, nextCmd := model.Update(msg)
	return testModel(t, updated), nextCmd
}

func stubModelCatalog(
	t *testing.T,
	fn func(context.Context, *config.Config) ([]llm.ModelMetadata, error),
) {
	t.Helper()
	oldListModelsForConfig := listModelsForConfig
	oldCachedModelsForConfig := cachedModelsForConfig
	listModelsForConfig = fn
	cachedModelsForConfig = func(*config.Config) ([]llm.ModelMetadata, bool, bool) {
		return nil, false, false
	}
	t.Cleanup(func() {
		listModelsForConfig = oldListModelsForConfig
		cachedModelsForConfig = oldCachedModelsForConfig
	})
}
