package workspace

import (
	"context"
	"sync"
)

// SearchHit is a ranked match returned from a workspace search index.
type SearchHit struct {
	Ref   ContentRef
	Score int
}

// SearchIndex stores workspace documents in a searchable form.
//
// The current concrete implementation is a trigram index over rooted workspace
// paths plus content. The interface stays small so other index algorithms can
// replace it later without changing callers that only need upsert/search.
type SearchIndex interface {
	Upsert(ctx context.Context, ref ContentRef, data []byte) error
	Delete(ctx context.Context, path string) error
	Search(ctx context.Context, query string, limit int) ([]SearchHit, error)
}

// TrigramIndex is an in-memory trigram search substrate over workspace files.
//
// It keeps doc IDs stable per workspace path, stores only unique normalized
// trigrams per document, and uses sorted merge intersection at query time.
type TrigramIndex struct {
	mu       sync.RWMutex
	nextID   uint32
	docs     map[string]*trigramDoc
	byID     map[uint32]*trigramDoc
	postings map[string][]uint32
}

type trigramDoc struct {
	id    uint32
	ref   ContentRef
	terms []string
}

// NewSearchIndex returns the default workspace search substrate.
func NewSearchIndex() *TrigramIndex {
	return &TrigramIndex{
		docs:     make(map[string]*trigramDoc),
		byID:     make(map[uint32]*trigramDoc),
		postings: make(map[string][]uint32),
	}
}

var _ SearchIndex = (*TrigramIndex)(nil)
