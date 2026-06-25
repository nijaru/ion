package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"fmt"

	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/internal/core"
)

type UnconfiguredBackend struct {
	cfg     *config.Config
	session *unconfiguredSession
}

func NewUnconfigured(cfg *config.Config, reason error) *UnconfiguredBackend {
	return &UnconfiguredBackend{
		cfg:     cfg,
		session: newUnconfiguredSession(reason),
	}
}

func (b *UnconfiguredBackend) Name() string {
	return "unconfigured"
}

func (b *UnconfiguredBackend) Provider() string {
	if b.cfg == nil {
		return ""
	}
	return b.cfg.Provider
}

func (b *UnconfiguredBackend) Model() string {
	if b.cfg == nil {
		return ""
	}
	return b.cfg.Model
}

func (b *UnconfiguredBackend) ContextLimit() int {
	if b.cfg == nil {
		return 0
	}
	return b.cfg.ContextLimit
}

func (b *UnconfiguredBackend) Bootstrap() core.Bootstrap {
	status := "Provider and model are required. Use /provider, then /model."
	if b.session.reason != nil {
		status = b.session.reason.Error()
	}
	return core.Bootstrap{
		Entries: []session.Entry{},
		Status:  status,
	}
}

func (b *UnconfiguredBackend) Session() session.Session {
	return b.session
}

func (b *UnconfiguredBackend) SetStore(session.Store) {}

func (b *UnconfiguredBackend) SetSession(s session.Session) {
	b.session.setStorage(s)
}

func (b *UnconfiguredBackend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

type unconfiguredSession struct {
	events chan session.Event
	reason error
	id     string
	meta   session.Metadata
}

func newUnconfiguredSession(reason error) *unconfiguredSession {
	return &unconfiguredSession{
		events: make(chan session.Event, 10),
		reason: reason,
		meta:   session.Metadata{},
	}
}

func (s *unconfiguredSession) setStorage(storageSession session.Session) {
	if storageSession == nil {
		return
	}
	s.id = storageSession.ID()
	meta := storageSession.Meta()
	s.meta = session.Metadata{
		Model:  meta.Model,
		Branch: meta.Branch,
		CWD:    meta.CWD,
	}
}

func (s *unconfiguredSession) Open(context.Context) error {
	return nil
}

func (s *unconfiguredSession) Resume(context.Context, string) error {
	return nil
}

func (s *unconfiguredSession) SubmitTurn(context.Context, string) error {
	err := s.reason
	if err == nil {
		err = fmt.Errorf("ion is not configured yet")
	}
	return err
}

func (s *unconfiguredSession) CancelTurn(context.Context) error {
	return nil
}

func (s *unconfiguredSession) Close() error {
	return nil
}

func (s *unconfiguredSession) Events() <-chan session.Event {
	return s.events
}

func (s *unconfiguredSession) EventSender() chan<- session.Event {
	return s.events
}

func (s *unconfiguredSession) ID() string {
	return s.id
}

func (s *unconfiguredSession) Meta() session.Metadata {
	return s.meta
}

func (s *unconfiguredSession) Append(ctx context.Context, entry session.Entry) (string, error) {
	return "", nil
}

func (s *unconfiguredSession) AppendBranchSummary(ctx context.Context, data session.BranchSummaryData) (string, error) {
	return "", nil
}
func (s *unconfiguredSession) AppendLabel(ctx context.Context, targetID, label string) (string, error) {
	return "", nil
}
func (s *unconfiguredSession) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	return "", nil
}
func (s *unconfiguredSession) AppendCustom(ctx context.Context, entry *session.CustomEntry) (string, error) {
	return "", nil
}
func (s *unconfiguredSession) GetEntry(ctx context.Context, id string) (session.Entry, error) {
	return nil, nil
}
func (s *unconfiguredSession) GetLeafID() string { return "" }
func (s *unconfiguredSession) SetLeafID(id string) error { return nil }
func (s *unconfiguredSession) MoveTo(ctx context.Context, leafID string, summary *session.BranchSummaryData) (string, error) {
	return "", nil
}

func (s *unconfiguredSession) AppendCompaction(ctx context.Context, data session.CompactionData) (string, error) {
	return "", nil
}

func (s *unconfiguredSession) AppendMessage(ctx context.Context, msg session.Message) (string, error) {
	return "", nil
}
func (s *unconfiguredSession) AppendModelChange(ctx context.Context, provider, modelID string) (string, error) {
	return "", nil
}
func (s *unconfiguredSession) AppendThinkingChange(ctx context.Context, level session.ThinkingLevel) (string, error) {
	return "", nil
}
func (s *unconfiguredSession) AppendToolsChange(ctx context.Context, tools []string) (string, error) {
	return "", nil
}

func (s *unconfiguredSession) Branch(ctx context.Context) ([]session.Entry, error) {
	return nil, nil
}

func (s *unconfiguredSession) BuildContext(ctx context.Context) (session.ContextSnapshot, error) {
	return session.ContextSnapshot{}, nil
}


func (s *unconfiguredSession) Usage(ctx context.Context) (session.Usage, error) { return session.Usage{}, nil }

func (s *unconfiguredSession) Entries(ctx context.Context) ([]session.Entry, error) { return nil, nil }
