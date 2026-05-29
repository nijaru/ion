package session

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"slices"
	"strings"
	"time"

	"github.com/go-json-experiment/json"
)

// Parent returns the persisted ancestry record for the parent of sessionID.
func (s *JSONLStore) Parent(ctx context.Context, sessionID string) (*SessionAncestry, error) {
	s.ancestryMu.RLock()
	defer s.ancestryMu.RUnlock()

	index, err := s.loadAncestryIndexLocked()
	if err != nil {
		return nil, err
	}
	record, ok := index[sessionID]
	if !ok {
		return nil, fmt.Errorf("session ancestry %q not found", sessionID)
	}
	if record.ParentSessionID == "" {
		return nil, nil
	}
	parent, ok := index[record.ParentSessionID]
	if !ok {
		return nil, fmt.Errorf("session ancestry %q not found", record.ParentSessionID)
	}
	return &parent, nil
}

// Children lists the persisted ancestry records for direct children of sessionID.
func (s *JSONLStore) Children(ctx context.Context, sessionID string) ([]SessionAncestry, error) {
	s.ancestryMu.RLock()
	defer s.ancestryMu.RUnlock()

	index, err := s.loadAncestryIndexLocked()
	if err != nil {
		return nil, err
	}
	children := make([]SessionAncestry, 0, 8)
	for _, record := range index {
		if record.ParentSessionID == sessionID {
			children = append(children, record)
		}
	}
	slices.SortFunc(children, func(a, b SessionAncestry) int {
		if cmp := a.CreatedAt.Compare(b.CreatedAt); cmp != 0 {
			return cmp
		}
		return strings.Compare(a.SessionID, b.SessionID)
	})
	return children, nil
}

// Lineage returns the root-to-current ancestry chain for sessionID.
func (s *JSONLStore) Lineage(ctx context.Context, sessionID string) ([]SessionAncestry, error) {
	s.ancestryMu.RLock()
	defer s.ancestryMu.RUnlock()

	index, err := s.loadAncestryIndexLocked()
	if err != nil {
		return nil, err
	}
	return lineageFromMap(sessionID, index)
}

// SaveAncestry persists existing ancestry metadata for portable session
// imports.
func (s *JSONLStore) SaveAncestry(_ context.Context, record SessionAncestry) error {
	if err := validateSessionAncestry(record); err != nil {
		return err
	}
	s.ancestryMu.Lock()
	defer s.ancestryMu.Unlock()
	return s.appendAncestryLocked(record)
}

func (s *JSONLStore) ensureRootAncestryLocked(sessionID string, createdAt time.Time) (int, error) {
	index, err := s.loadAncestryIndexLocked()
	if err != nil {
		return 0, err
	}
	if record, ok := index[sessionID]; ok {
		return record.Depth, nil
	}
	record := SessionAncestry{
		SessionID: sessionID,
		Depth:     0,
		CreatedAt: createdAt,
	}
	if err := s.appendAncestryRecordLocked(record); err != nil {
		return 0, err
	}
	return 0, nil
}

func (s *JSONLStore) appendAncestryLocked(record SessionAncestry) error {
	index, err := s.loadAncestryIndexLocked()
	if err != nil {
		return err
	}
	if _, exists := index[record.SessionID]; exists {
		return nil
	}
	return s.appendAncestryRecordLocked(record)
}

func (s *JSONLStore) appendAncestryRecordLocked(record SessionAncestry) error {
	path := "ancestry.jsonl"
	f, err := s.root.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()

	if err := json.MarshalWrite(f, record); err != nil {
		return err
	}
	_, err = f.Write([]byte("\n"))
	return err
}

func (s *JSONLStore) loadAncestryIndexLocked() (map[string]SessionAncestry, error) {
	path := "ancestry.jsonl"
	f, err := s.root.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return make(map[string]SessionAncestry), nil
		}
		return nil, err
	}
	defer f.Close()

	index := make(map[string]SessionAncestry)
	reader := bufio.NewReader(f)
	for {
		line, readErr := reader.ReadBytes('\n')
		if readErr != nil && readErr != io.EOF {
			return nil, readErr
		}
		line = bytes.TrimSpace(line)
		if len(line) == 0 {
			if readErr == io.EOF {
				break
			}
			continue
		}

		var record SessionAncestry
		if err := json.Unmarshal(line, &record); err != nil {
			return nil, err
		}
		index[record.SessionID] = record
		if readErr == io.EOF {
			break
		}
	}
	return index, nil
}
