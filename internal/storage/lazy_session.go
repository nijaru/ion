package storage

import (
	"context"
	"fmt"
	"sync"
	"time"

	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/llm"
)

type materializedSession interface {
	Materialized() bool
}

type sessionOpenerWithID interface {
	OpenSessionWithID(ctx context.Context, id, cwd, model, branch string) (Session, error)
}

type LazySession struct {
	mu      sync.Mutex
	store   Store
	id      string
	meta    Metadata
	created Session
}

func NewLazySession(store Store, cwd, model, branch string) *LazySession {
	now := time.Now()
	id := fmt.Sprintf("%d-%s", now.Unix(), ionsession.ShortID())
	return &LazySession{
		store: store,
		id:    id,
		meta: Metadata{
			ID:        id,
			CWD:       cwd,
			Model:     model,
			Branch:    branch,
			CreatedAt: now,
		},
	}
}

func (s *LazySession) ID() string { return s.id }

func (s *LazySession) Meta() Metadata { return s.meta }

func (s *LazySession) Materialized() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.created != nil
}

func (s *LazySession) Ensure(ctx context.Context) (Session, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.created != nil {
		return s.created, nil
	}
	if s.store == nil {
		return nil, fmt.Errorf("open lazy session %s: storage store is not configured", s.id)
	}
	var (
		created Session
		err     error
	)
	if opener, ok := s.store.(sessionOpenerWithID); ok {
		created, err = opener.OpenSessionWithID(ctx, s.id, s.meta.CWD, s.meta.Model, s.meta.Branch)
	} else {
		created, err = s.store.OpenSession(ctx, s.meta.CWD, s.meta.Model, s.meta.Branch)
	}
	if err != nil {
		return nil, err
	}
	s.created = created
	s.id = created.ID()
	s.meta = created.Meta()
	return created, nil
}

func (s *LazySession) Append(ctx context.Context, event Event) error {
	s.mu.Lock()
	created := s.created
	s.mu.Unlock()
	if created == nil {
		return nil
	}
	return created.Append(ctx, event)
}

func (s *LazySession) AppendModelMessage(ctx context.Context, message llm.Message) error {
	if isEmptyModelMessage(message) {
		return nil
	}
	created, err := s.Ensure(ctx)
	if err != nil {
		return err
	}
	writer, ok := created.(interface {
		AppendModelMessage(context.Context, llm.Message) error
	})
	if !ok {
		return nil
	}
	return writer.AppendModelMessage(ctx, message)
}

func (s *LazySession) ModelMessages(ctx context.Context) ([]llm.Message, error) {
	s.mu.Lock()
	created := s.created
	s.mu.Unlock()
	if created == nil {
		return nil, nil
	}
	reader, ok := created.(interface {
		ModelMessages(context.Context) ([]llm.Message, error)
	})
	if !ok {
		return nil, nil
	}
	return reader.ModelMessages(ctx)
}

func (s *LazySession) Entries(ctx context.Context) ([]ionsession.Entry, error) {
	s.mu.Lock()
	created := s.created
	s.mu.Unlock()
	if created == nil {
		return nil, nil
	}
	return created.Entries(ctx)
}

func (s *LazySession) LastStatus(ctx context.Context) (string, error) {
	s.mu.Lock()
	created := s.created
	s.mu.Unlock()
	if created == nil {
		return "", nil
	}
	return created.LastStatus(ctx)
}

func (s *LazySession) Usage(ctx context.Context) (int, int, float64, error) {
	s.mu.Lock()
	created := s.created
	s.mu.Unlock()
	if created == nil {
		return 0, 0, 0, nil
	}
	return created.Usage(ctx)
}

func (s *LazySession) Close() error {
	s.mu.Lock()
	created := s.created
	s.mu.Unlock()
	if created == nil {
		return nil
	}
	return created.Close()
}

func IsMaterialized(sess Session) bool {
	if sess == nil {
		return false
	}
	if m, ok := sess.(materializedSession); ok {
		return m.Materialized()
	}
	return true
}
