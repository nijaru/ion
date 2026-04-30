package storage

import (
	"context"
	"fmt"
	"sync"
	"time"

	ionsession "github.com/nijaru/ion/internal/session"
)

type materializedSession interface {
	Materialized() bool
}

type LazySession struct {
	mu      sync.Mutex
	store   Store
	id      string
	meta    Metadata
	created Session
}

func NewLazySession(store Store, cwd, model, branch string) *LazySession {
	id := fmt.Sprintf("%d-%s", time.Now().Unix(), ionsession.ShortID())
	return &LazySession{
		store: store,
		id:    id,
		meta: Metadata{
			ID:        id,
			CWD:       cwd,
			Model:     model,
			Branch:    branch,
			CreatedAt: time.Now(),
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
	created, err := s.store.OpenSession(ctx, s.meta.CWD, s.meta.Model, s.meta.Branch)
	if err != nil {
		return nil, err
	}
	s.created = created
	s.id = created.ID()
	s.meta = created.Meta()
	return created, nil
}

func (s *LazySession) Append(ctx context.Context, event any) error {
	if isNoopAppendEvent(event) {
		return nil
	}
	s.mu.Lock()
	created := s.created
	s.mu.Unlock()
	if created == nil {
		return nil
	}
	return created.Append(ctx, event)
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
