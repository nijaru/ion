package session

import (
	"os"
	"sync"
)

// JSONLStore is a file-backed store that saves events as JSON lines.
type JSONLStore struct {
	root       *os.Root
	ancestryMu sync.RWMutex
	sessionMus sync.Map // map[string]*sync.RWMutex
}

// NewJSONLStore creates a new JSONL store rooted at dir.
func NewJSONLStore(dir string) (*JSONLStore, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, err
	}
	return &JSONLStore{root: root}, nil
}

// Close releases the underlying filesystem root.
func (s *JSONLStore) Close() error {
	if s == nil || s.root == nil {
		return nil
	}
	return s.root.Close()
}

func (s *JSONLStore) getSessionMu(sessionID string) *sync.RWMutex {
	mu, _ := s.sessionMus.LoadOrStore(sessionID, &sync.RWMutex{})
	return mu.(*sync.RWMutex)
}
