package app

import (
	"context"
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

// --- stubSession implements session.Session ---

type stubSession struct {
	id         string
	events     chan session.Event
	submits    []string
	cancels    int
	submitErr  error
	closed     bool
	messages   []session.Message
	entries    []session.Entry
	leafID     string
}

func newStubSession(id string) *stubSession {
	return &stubSession{
		id:     id,
		events: make(chan session.Event, 100),
	}
}

func (s *stubSession) ID() string              { return s.id }
func (s *stubSession) Meta() session.Metadata   { return session.Metadata{ID: s.id} }

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
func (s *stubSession) AppendThinkingChange(_ context.Context, _ session.ThinkingLevel) (string, error) {
	return "tc-1", nil
}
func (s *stubSession) AppendToolsChange(_ context.Context, _ []string) (string, error) {
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

func (s *stubSession) Events() <-chan session.Event   { return s.events }
func (s *stubSession) EventSender() chan session.Event { return s.events }

func (s *stubSession) GetEntry(_ context.Context, _ string) (session.Entry, error) {
	return nil, nil
}
func (s *stubSession) GetLeafID() string       { return s.leafID }
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

// --- stubBackend implements agent.Backend ---

type stubBackend struct {
	sess         session.Session
	provider     string
	model        string
	providerSet  bool
	modelSet     bool
	contextLimit int
	surface      agent.ToolSurface
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
func (b stubBackend) Bootstrap() agent.Bootstrap {
	return agent.Bootstrap{
		Entries: []session.Entry{&session.LabelEntry{EntryBase: session.EntryBase{ID: "boot"}, Label: "boot"}},
		Status:  "ready",
	}
}
func (b stubBackend) Session() session.Session { return b.sess }
func (b stubBackend) SetStore(_ session.Store)  {}
func (b stubBackend) SetConfig(_ *config.Config) {}

// --- configCaptureBackend ---

type configCaptureBackend struct {
	stubBackend
	cfg *config.Config
}

func (b *configCaptureBackend) SetConfig(cfg *config.Config) {
	if cfg == nil {
		b.cfg = nil
		return
	}
	copied := *cfg
	b.cfg = &copied
}

// --- compactBackend ---

type compactBackend struct {
	stubBackend
	compacted bool
	err       error
	called    bool
}

func (b *compactBackend) Compact(_ context.Context) (bool, error) {
	b.called = true
	return b.compacted, b.err
}

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

// --- stubStorageSession tracks appends for persistence tests ---

type stubStorageSession struct {
	stubSession
	appends    []session.Entry
	appendErr  error
}

func (s *stubStorageSession) Append(_ context.Context, entry session.Entry) (string, error) {
	if s.appendErr != nil {
		return "", s.appendErr
	}
	s.appends = append(s.appends, entry)
	return "appended-1", nil
}

// --- Entry constructors for tests ---

// sysEntry creates a StoreSystem entry (implements session.Entry).
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

func sysEntry(content string) session.StoreSystem {
	return session.StoreSystem{Type: "system", Content: content}
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
func userMsgEntry(text string) *session.MessageEntry {
	return &session.MessageEntry{
		Message: &session.UserMessage{Content: []session.Content{session.TextContent{Text: text}}},
	}
}

func containsSessionEvent[T session.Event](messages []tea.Msg) bool {
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

// entryFromRole creates a session.Entry from role-based fields (compatibility helper).
// Used by tests that were written against the old session.Entry struct.
func entryFromRole(role, title, content string, isError bool) session.Entry {
	switch role {
	case "tool":
		return toolEntry(title, content, isError)
	case "user":
		return userMsgEntry(content)
	case "agent":
		return agentMsgEntry(content)
	default:
		return sysEntry(content)
	}
}

// oldEntry is a compatibility struct that matches the old session.Entry fields.
// Used to convert old-style struct literals to the new Entry interface.
type oldEntry struct {
	Role      string
	Title     string
	Content   string
	Reasoning string
	IsError   bool
}

// toEntry converts an oldEntry to a session.Entry interface.
func (e oldEntry) toEntry() session.Entry {
	switch e.Role {
	case "tool":
		return toolEntry(e.Title, e.Content, e.IsError)
	case "user":
		return userMsgEntry(e.Content)
	case "agent":
		return agentMsgEntry(e.Content)
	default:
		return sysEntry(e.Content)
	}
}
