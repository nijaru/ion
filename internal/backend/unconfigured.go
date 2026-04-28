package backend

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
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

func (b *UnconfiguredBackend) Bootstrap() Bootstrap {
	status := "Provider and model are required. Use /provider, then /model."
	if b.session.reason != nil {
		status = b.session.reason.Error()
	}
	return Bootstrap{
		Entries: []session.Entry{},
		Status:  status,
	}
}

func (b *UnconfiguredBackend) Session() session.AgentSession {
	return b.session
}

func (b *UnconfiguredBackend) SetStore(storage.Store) {}

func (b *UnconfiguredBackend) SetSession(storage.Session) {}

func (b *UnconfiguredBackend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

type unconfiguredSession struct {
	events chan session.Event
	reason error
}

func newUnconfiguredSession(reason error) *unconfiguredSession {
	return &unconfiguredSession{
		events: make(chan session.Event, 10),
		reason: reason,
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

func (s *unconfiguredSession) Approve(context.Context, string, bool) error {
	return nil
}

func (s *unconfiguredSession) RegisterMCPServer(context.Context, string, ...string) error {
	return nil
}

func (s *unconfiguredSession) SetMode(session.Mode) {}

func (s *unconfiguredSession) SetAutoApprove(bool) {}

func (s *unconfiguredSession) AllowCategory(string) {}

func (s *unconfiguredSession) Close() error {
	return nil
}

func (s *unconfiguredSession) Events() <-chan session.Event {
	return s.events
}

func (s *unconfiguredSession) ID() string {
	return ""
}

func (s *unconfiguredSession) Meta() map[string]string {
	return map[string]string{}
}
