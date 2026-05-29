package workspace

import (
	"cmp"
	"context"
	"fmt"
	"slices"
	"strings"
)

// Search returns ranked hits for the query across indexed workspace files.
func (i *TrigramIndex) Search(ctx context.Context, query string, limit int) ([]SearchHit, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if i == nil {
		return nil, fmt.Errorf("workspace search index: nil index")
	}
	if limit < 0 {
		limit = 0
	}

	query = strings.ToLower(strings.TrimSpace(query))
	if query == "" {
		return nil, nil
	}

	i.mu.RLock()
	defer i.mu.RUnlock()

	terms := queryTerms(query)
	if len(terms) == 0 {
		return i.searchShortLocked(query, limit)
	}

	type termPosting struct {
		term    string
		posting []uint32
	}
	postings := make([]termPosting, 0, len(terms))
	for _, term := range terms {
		list := i.postings[term]
		if len(list) == 0 {
			return nil, nil
		}
		postings = append(postings, termPosting{term: term, posting: list})
	}
	slices.SortFunc(postings, func(a, b termPosting) int {
		return cmp.Compare(len(a.posting), len(b.posting))
	})

	candidates := slices.Clone(postings[0].posting)
	for _, posting := range postings[1:] {
		candidates = intersectSorted(candidates, posting.posting)
		if len(candidates) == 0 {
			return nil, nil
		}
	}

	hits := make([]SearchHit, 0, len(candidates))
	for _, id := range candidates {
		doc := i.byID[id]
		if doc == nil {
			continue
		}
		hits = append(hits, SearchHit{
			Ref:   doc.ref,
			Score: scoreHit(query, doc.ref, len(terms)),
		})
	}

	sortSearchHits(hits)
	if limit > 0 && len(hits) > limit {
		hits = hits[:limit]
	}
	return hits, nil
}

func (i *TrigramIndex) searchShortLocked(query string, limit int) ([]SearchHit, error) {
	hits := make([]SearchHit, 0, len(i.docs))
	for _, doc := range i.docs {
		score := 0
		if strings.Contains(strings.ToLower(doc.ref.Path), query) {
			score = 1 + len(query)
		}
		if score == 0 {
			continue
		}
		hits = append(hits, SearchHit{Ref: doc.ref, Score: score})
	}
	sortSearchHits(hits)
	if limit > 0 && len(hits) > limit {
		hits = hits[:limit]
	}
	return hits, nil
}

func sortSearchHits(hits []SearchHit) {
	slices.SortFunc(hits, func(a, b SearchHit) int {
		if a.Score != b.Score {
			return cmp.Compare(b.Score, a.Score)
		}
		return strings.Compare(a.Ref.Path, b.Ref.Path)
	})
}
