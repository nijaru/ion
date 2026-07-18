package app

import (
	"context"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// --- stubSession implements session.Session ---

type stubSession struct {
	id        string
	name      string
	events    chan session.Event
	submits   []string
	cancels   int
	submitErr error
	closed    bool
	messages  []session.Message
	entries   []session.Entry
	leafID    string
}

func newStubSession(id string) *stubSession {
	return &stubSession{
		id:     id,
		events: make(chan session.Event, 100),
	}
}

func (s *stubSession) ID() string                         { return s.id }
func (s *stubSession) Meta() session.Metadata             { return session.Metadata{ID: s.id} }
func (s *stubSession) SessionName(context.Context) string { return s.name }

func (s *stubSession) BuildContext(_ context.Context) (session.ContextSnapshot, error) {
	return session.ContextSnapshot{Messages: s.messages}, nil
}

func (s *stubSession) Branch(_ context.Context) ([]session.Entry, error) {
	return s.entries, nil
}

func (s *stubSession) AppendMessage(_ context.Context, msg session.Message) (string, error) {
	s.messages = append(s.messages, msg)
	return "msg-1", nil
}

func (s *stubSession) AppendModelChange(_ context.Context, _, _ string) (string, error) {
	return "mc-1", nil
}
func (s *stubSession) AppendThinkingLevelChange(_ context.Context, _ session.ThinkingLevel) (string, error) {
	return "tc-1", nil
}
func (s *stubSession) AppendActiveToolsChange(_ context.Context, _ []string) (string, error) {
	return "tools-1", nil
}
func (s *stubSession) AppendCompaction(_ context.Context, _ session.CompactionData) (string, error) {
	return "compact-1", nil
}
func (s *stubSession) AppendBranchSummary(_ context.Context, _ session.BranchSummaryData) (string, error) {
	return "bs-1", nil
}
func (s *stubSession) AppendLabel(_ context.Context, _, _ string) (string, error) {
	return "label-1", nil
}
func (s *stubSession) AppendSessionInfo(_ context.Context, _ string) (string, error) {
	return "si-1", nil
}
func (s *stubSession) AppendCustom(_ context.Context, _ *session.CustomEntry) (string, error) {
	return "custom-1", nil
}
func (s *stubSession) AppendLeaf(_ context.Context, _ string) (string, error) {
	return "leaf-1", nil
}
func (s *stubSession) AppendCustomMessage(_ context.Context, _ *session.CustomMessageEntry) (string, error) {
	return "cm-1", nil
}
func (s *stubSession) GetLabel(_ context.Context, _ string) (string, error) {
	return "", nil
}
func (s *stubSession) Append(_ context.Context, _ session.Entry) (string, error) {
	return "entry-1", nil
}

func (s *stubSession) SubmitTurn(_ context.Context, text string) error {
	if s.submitErr != nil {
		return s.submitErr
	}
	s.submits = append(s.submits, text)
	return nil
}

func (s *stubSession) CancelTurn(_ context.Context) error {
	s.cancels++
	return nil
}

func (s *stubSession) Events() <-chan session.Event    { return s.events }
func (s *stubSession) EventSender() chan session.Event { return s.events }

func (s *stubSession) GetEntry(_ context.Context, _ string) (session.Entry, error) {
	return nil, nil
}
func (s *stubSession) GetLeafID() string         { return s.leafID }
func (s *stubSession) SetLeafID(id string) error { s.leafID = id; return nil }
func (s *stubSession) MoveTo(_ context.Context, _ string, _ *session.BranchSummaryData) (string, error) {
	return "", nil
}
func (s *stubSession) Entries(_ context.Context) ([]session.Entry, error) {
	return s.entries, nil
}
func (s *stubSession) Usage(_ context.Context) (session.Usage, error) {
	return session.Usage{}, nil
}
func (s *stubSession) Close() error {
	s.closed = true
	if s.events != nil {
		close(s.events)
		s.events = nil
	}
	return nil
}

// --- stubRunner implements agent.Runner ---

type stubRunner struct {
	aborts       int
	compacts     int
	promptTexts  []string
	promptImages [][]session.ImageContent
	promptErr    error
	appends      []session.Entry
	appendErr    error
	navigates    int
	navigateID   string
	navigateOpts agent.NavigateOptions
	navigateErr  error
	thinking     []session.ThinkingLevel
	thinkingErr  error
}

func (r *stubRunner) Events() <-chan session.Event { return nil }
func (r *stubRunner) Prompt(_ context.Context, text string, images ...session.ImageContent) (session.Message, error) {
	r.promptTexts = append(r.promptTexts, text)
	r.promptImages = append(r.promptImages, cloneImageAttachments(images))
	return nil, r.promptErr
}
func (r *stubRunner) Steer(_ string, _ ...session.ImageContent) error    { return nil }
func (r *stubRunner) FollowUp(_ string, _ ...session.ImageContent) error { return nil }
func (r *stubRunner) NextTurn(_ string, _ ...session.ImageContent)       {}
func (r *stubRunner) Abort() ([]session.Message, []session.Message, error) {
	r.aborts++
	return nil, nil, nil
}
func (r *stubRunner) WaitForIdle()             {}
func (r *stubRunner) Close() error             { return nil }
func (r *stubRunner) Session() session.Session { return nil }
func (r *stubRunner) SetModel(_ llm.Model)     {}
func (r *stubRunner) SetThinking(_ context.Context, level session.ThinkingLevel) error {
	if r.thinkingErr != nil {
		return r.thinkingErr
	}
	r.thinking = append(r.thinking, level)
	return nil
}
func (r *stubRunner) SetTools(_ []agent.Tool, _ []string)           {}
func (r *stubRunner) ActivateTools(context.Context, []string) error { return nil }
func (r *stubRunner) PersistEntry(_ context.Context, entry session.Entry) error {
	if r.appendErr != nil {
		return r.appendErr
	}
	r.appends = append(r.appends, entry)
	return nil
}
func (r *stubRunner) AppendSessionInfo(context.Context, string) (string, error) { return "", nil }
func (r *stubRunner) ForkSession(context.Context, string) (string, error)       { return "", nil }
func (r *stubRunner) ImportSessionBundle(context.Context, ionexport.SessionBundle) (string, error) {
	return "", nil
}
func (r *stubRunner) NavigateTree(_ context.Context, targetID string, opts agent.NavigateOptions) (agent.NavigateResult, error) {
	r.navigates++
	r.navigateID = targetID
	r.navigateOpts = opts
	return agent.NavigateResult{}, r.navigateErr
}
func (r *stubRunner) AppendLabel(context.Context, string, string) (string, error) {
	return "", nil
}
func (r *stubRunner) GetLabel(context.Context, string) (string, error) { return "", nil }
func (r *stubRunner) Compact(_ context.Context) error {
	r.compacts++
	return nil
}

// --- stubBackend implements RuntimeInfo ---

type stubBackend struct {
	sess         session.Session
	provider     string
	model        string
	providerSet  bool
	modelSet     bool
	contextLimit int
	surface      ToolSurface
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
func (b stubBackend) ContextLimit() int { return b.contextLimit }
func (b stubBackend) Bootstrap() Bootstrap {
	return Bootstrap{
		Entries: []session.Entry{&session.LabelEntry{EntryBase: session.EntryBase{ID: "boot"}, Label: "boot"}},
		Status:  "ready",
	}
}
func (b stubBackend) Session() session.Session { return b.sess }

// --- readyModel creates a Model ready for testing ---

func readyModel(t *testing.T) Model {
	t.Helper()
	if home, err := os.UserHomeDir(); err == nil {
		if !strings.Contains(home, "tmp") && !strings.Contains(home, "TempDir") &&
			!strings.Contains(home, "folders") &&
			!strings.Contains(home, "/var/") {
			t.Setenv("HOME", t.TempDir())
		}
	}
	sess := newStubSession("stub")
	b := stubBackend{sess: sess}
	model := New(b, nil, nil, "/tmp/test", "main", "dev", nil)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	return testModel(t, updated)
}

// --- testModel helper ---

func testModel(t testing.TB, updated any) Model {
	t.Helper()
	switch next := updated.(type) {
	case Model:
		return next
	case *Model:
		if next == nil {
			t.Fatal("updated model is nil")
			return Model{}
		}
		return *next
	default:
		t.Fatalf("updated model = %T, want Model", updated)
		return Model{}
	}
}

// --- command helpers ---

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

type stubStorageSession struct {
	stubSession
	appends       []session.Entry
	appendErr     error
	storageID     string
	storageModel  string
	storageBranch string
}

func (s *stubStorageSession) Append(_ context.Context, entry session.Entry) (string, error) {
	if s.appendErr != nil {
		return "", s.appendErr
	}
	s.appends = append(s.appends, entry)
	return "appended-1", nil
}

// --- Entry constructors for tests ---

// sysEntry creates a durable system entry for terminal-commit tests.
// toolEntry creates a MessageEntry wrapping a ToolResultMessage.
func toolEntry(title, content string, isError bool) *session.MessageEntry {
	return &session.MessageEntry{
		Message: &session.ToolResultMessage{
			ToolName: title,
			Title:    title,
			Content:  []session.Content{session.TextContent{Text: content}},
			IsError:  isError,
		},
	}
}

func sysEntry(content string) session.Entry {
	entry, err := session.EntrySystem(content, time.Time{})
	if err != nil {
		panic(err)
	}
	return entry
}

// agentMsgEntry creates a MessageEntry wrapping an AssistantMessage.
func agentMsgEntry(text string) *session.MessageEntry {
	return &session.MessageEntry{
		Message: &session.AssistantMessage{
			Content: []session.Content{session.TextContent{Text: text}},
		},
	}
}

// userMsgEntry creates a MessageEntry wrapping a UserMessage.
// readyModelWithSwitcher creates a model with a switcher that records model switches.
func readyModelWithSwitcher(t *testing.T, observed *[]string) Model {
	t.Helper()
	sess := newStubSession("stub")
	b := stubBackend{sess: sess, provider: "openai", model: "gpt-4.1"}
	switcher := func(ctx context.Context, cfg *config.Config, sessionID string) (RuntimeInfo, agent.Runner, session.Session, error) {
		*observed = append(*observed, cfg.Model)
		newSess := newStubSession(sessionID)
		newBackend := stubBackend{sess: newSess, provider: cfg.Provider, model: cfg.Model}
		return newBackend, &stubRunner{}, newSess, nil
	}
	model := New(b, nil, nil, "/tmp/test", "main", "dev", switcher)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	return testModel(t, updated)
}

// resumeOnlyStore is a minimal store stub for tests that don't need persistence.
type resumeOnlyStore struct{}

func (s *resumeOnlyStore) Append(ctx context.Context, entry session.Entry) (string, error) {
	return "id-" + time.Now().Format("150405"), nil
}
func (s *resumeOnlyStore) AppendBatch(ctx context.Context, entries []session.Entry) ([]string, error) {
	ids := make([]string, len(entries))
	for i := range entries {
		ids[i] = "id-" + time.Now().Format("150405")
	}
	return ids, nil
}
func (s *resumeOnlyStore) GetEntry(ctx context.Context, id string) (session.Entry, error) {
	return nil, nil
}
func (s *resumeOnlyStore) Branch(ctx context.Context) ([]session.Entry, error) {
	return nil, nil
}
func (s *resumeOnlyStore) Entries(ctx context.Context) ([]session.Entry, error) {
	return nil, nil
}
func (s *resumeOnlyStore) GetLeafID() string         { return "" }
func (s *resumeOnlyStore) SetLeafID(id string) error { return nil }
func (s *resumeOnlyStore) Meta() session.Metadata    { return session.Metadata{} }
func (s *resumeOnlyStore) GetInputs(ctx context.Context, workdir string, n int) ([]string, error) {
	return nil, nil
}
func (s *resumeOnlyStore) ListSessions(ctx context.Context, workdir string) ([]session.SessionInfoEntry, error) {
	return nil, nil
}
func (s *resumeOnlyStore) UpdateSession(ctx context.Context, info session.SessionInfoEntry) error {
	return nil
}
func (s *resumeOnlyStore) AppendLeafEntry(ctx context.Context, entry session.Entry) (string, error) {
	return s.Append(ctx, entry)
}
func (s *resumeOnlyStore) Close() error { return nil }
func (s *resumeOnlyStore) AddInput(ctx context.Context, workdir string, input string) error {
	return nil
}
