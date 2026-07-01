// Package testutil provides test stubs for the redesigned session/agent types.
package testutil

import (
	"context"
	"sync"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

// --- Mock Session ---

// MockSession implements session.Session for testing.
type MockSession struct {
	mu       sync.Mutex
	id       string
	meta     session.Metadata
	events   chan session.Event
	entries  []session.Entry
	leafID   string
	closed   bool
	messages []session.Message
}

// NewMockSession creates a MockSession with the given ID.
func NewMockSession(id string) *MockSession {
	return &MockSession{
		id:     id,
		meta:   session.Metadata{ID: id},
		events: make(chan session.Event, 100),
	}
}

func (s *MockSession) ID() string     { return s.id }
func (s *MockSession) Meta() session.Metadata { return s.meta }

func (s *MockSession) BuildContext(_ context.Context) (session.ContextSnapshot, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var msgs []session.Message
	msgs = append(msgs, s.messages...)
	return session.ContextSnapshot{Messages: msgs}, nil
}

func (s *MockSession) Branch(_ context.Context) ([]session.Entry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]session.Entry, len(s.entries))
	copy(out, s.entries)
	return out, nil
}

func (s *MockSession) AppendMessage(_ context.Context, msg session.Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	id := idOf(s.entries)
	s.entries = append(s.entries, &session.MessageEntry{EntryBase: session.EntryBase{ID: id}, Message: msg})
	s.messages = append(s.messages, msg)
	return id, nil
}

func (s *MockSession) AppendModelChange(_ context.Context, _, _ string) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) AppendThinkingChange(_ context.Context, _ session.ThinkingLevel) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) AppendToolsChange(_ context.Context, _ []string) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) AppendCompaction(_ context.Context, _ session.CompactionData) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) AppendBranchSummary(_ context.Context, _ session.BranchSummaryData) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) AppendLabel(_ context.Context, _, _ string) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) AppendSessionInfo(_ context.Context, _ string) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) AppendCustom(_ context.Context, _ *session.CustomEntry) (string, error) {
	return idOf(s.entries), nil
}
func (s *MockSession) Append(_ context.Context, _ session.Entry) (string, error) {
	return idOf(s.entries), nil
}

func (s *MockSession) Events() <-chan session.Event    { return s.events }
func (s *MockSession) EventSender() chan session.Event  { return s.events }

func (s *MockSession) GetEntry(_ context.Context, _ string) (session.Entry, error) {
	return nil, nil
}
func (s *MockSession) GetLeafID() string      { s.mu.Lock(); defer s.mu.Unlock(); return s.leafID }
func (s *MockSession) SetLeafID(id string) error { s.mu.Lock(); defer s.mu.Unlock(); s.leafID = id; return nil }
func (s *MockSession) MoveTo(_ context.Context, _ string, _ *session.BranchSummaryData) (string, error) {
	return "", nil
}
func (s *MockSession) Entries(_ context.Context) ([]session.Entry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]session.Entry, len(s.entries))
	copy(out, s.entries)
	return out, nil
}
func (s *MockSession) Usage(_ context.Context) (session.Usage, error) {
	return session.Usage{}, nil
}
func (s *MockSession) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closed = true
	close(s.events)
	return nil
}

// SendEvent injects an event into the session's event channel for testing.
func (s *MockSession) SendEvent(ev session.Event) {
	s.events <- ev
}

// Closed reports whether Close was called.
func (s *MockSession) Closed() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.closed
}

// Messages returns accumulated messages (for test assertions).
func (s *MockSession) Messages() []session.Message {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]session.Message, len(s.messages))
	copy(out, s.messages)
	return out
}

func idOf(entries []session.Entry) string {
	return "entry-" + time.Now().Format("20060102T150405.000")
}

// --- Mock Store ---

// MockStore implements session.Store for testing.
type MockStore struct {
	mu       sync.Mutex
	sessions []session.SessionInfoEntry
	entries  map[string][]session.Entry
	closed   bool
}

// NewMockStore creates a MockStore.
func NewMockStore() *MockStore {
	return &MockStore{entries: make(map[string][]session.Entry)}
}

func (s *MockStore) Append(_ context.Context, e session.Entry) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	id := e.ID()
	if id == "" {
		id = "store-" + time.Now().Format("20060102T150405.000")
	}
	return id, nil
}

func (s *MockStore) AppendLeafEntry(ctx context.Context, entry session.Entry) (string, error) {
	return s.Append(ctx, entry)
}

func (s *MockStore) GetEntry(_ context.Context, _ string) (session.Entry, error) {
	return nil, nil
}

func (s *MockStore) Branch(_ context.Context) ([]session.Entry, error) {
	return nil, nil
}

func (s *MockStore) Entries(_ context.Context) ([]session.Entry, error) {
	return nil, nil
}

func (s *MockStore) GetLeafID() string            { return "" }
func (s *MockStore) SetLeafID(_ string) error     { return nil }
func (s *MockStore) GetMetadata() session.Metadata { return session.Metadata{} }
func (s *MockStore) Meta() session.Metadata        { return session.Metadata{} }

func (s *MockStore) GetInputs(_ context.Context, _ string, _ int) ([]string, error) {
	return nil, nil
}

func (s *MockStore) ListSessions(_ context.Context, _ string) ([]session.SessionInfoEntry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]session.SessionInfoEntry, len(s.sessions))
	copy(out, s.sessions)
	return out, nil
}

func (s *MockStore) UpdateSession(_ context.Context, _ session.SessionInfoEntry) error {
	return nil
}

func (s *MockStore) AddInput(_ context.Context, _, _ string) error { return nil }

func (s *MockStore) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closed = true
	return nil
}

// AddSession adds a session info entry for ListSessions.
func (s *MockStore) AddSession(info session.SessionInfoEntry) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.sessions = append(s.sessions, info)
}

// --- Mock Backend ---

// ScriptStep is a pre-programmed event with optional delay.
type ScriptStep struct {
	Event session.Event
	Delay time.Duration
}

// MockBackend implements agent.Backend for testing.
// It also implements session.Session (the old testutil.Backend did this).
type MockBackend struct {
	mu          sync.Mutex
	NameVal     string
	ProviderVal string
	ModelVal    string
	CtxLimit    int
	BootstrapVal agent.Bootstrap
	SessionVal   session.Session
	StoreVal     session.Store
	ConfigVal    *config.Config
	script       []ScriptStep
	scriptPos    int
	events       chan session.Event
}

// New creates a MockBackend with default values.
func New() *MockBackend {
	return &MockBackend{
		events: make(chan session.Event, 100),
	}
}

// SetScript sets pre-programmed events to emit.
func (b *MockBackend) SetScript(steps []ScriptStep) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.script = steps
	b.scriptPos = 0
}

// SetSession sets the session to return from Session().
func (b *MockBackend) SetSession(s session.Session) {
	b.SessionVal = s
}

func (b *MockBackend) Name() string              { return b.NameVal }
func (b *MockBackend) Provider() string          { return b.ProviderVal }
func (b *MockBackend) Model() string             { return b.ModelVal }
func (b *MockBackend) ContextLimit() int         { return b.CtxLimit }
func (b *MockBackend) Bootstrap() agent.Bootstrap { return b.BootstrapVal }
func (b *MockBackend) Session() session.Session   { return b.SessionVal }
func (b *MockBackend) SetStore(s session.Store)   { b.StoreVal = s }
func (b *MockBackend) SetConfig(c *config.Config) {
	b.ConfigVal = c
	if c != nil {
		b.ProviderVal = c.Provider
		b.ModelVal = c.Model
	}
}

// --- Event Helpers ---

// TextDelta creates a TextDelta event for testing.
func TextDelta(text string) session.TextDelta {
	return session.TextDelta{Text: text}
}

// TurnStart creates a TurnStart event.
func TurnStart() session.TurnStart {
	return session.TurnStart{}
}

// TurnEnd creates a TurnEnd event.
func TurnEnd() session.TurnEnd {
	return session.TurnEnd{}
}

// MessageUpdate creates a MessageUpdate event with a TextDelta.
func MessageUpdate(text string) session.MessageUpdate {
	return session.MessageUpdate{Delta: session.TextDelta{Text: text}}
}

// --- Entry Helpers ---

// UserEntry creates a MessageEntry with a UserMessage.
func UserEntry(text string) *session.MessageEntry {
	return &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "user-" + time.Now().Format("150405.000")},
		Message:   session.UserMessage{Content: []session.Content{session.TextContent{Text: text}}},
	}
}

// AssistantEntry creates a MessageEntry with an AssistantMessage.
func AssistantEntry(text string) *session.MessageEntry {
	return &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "asst-" + time.Now().Format("150405.000")},
		Message:   session.AssistantMessage{Content: []session.Content{session.TextContent{Text: text}}},
	}
}
