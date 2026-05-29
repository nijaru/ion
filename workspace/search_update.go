package workspace

import (
	"context"
	"fmt"
)

// Upsert materializes one workspace file into the trigram index.
func (i *TrigramIndex) Upsert(ctx context.Context, ref ContentRef, data []byte) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if i == nil {
		return fmt.Errorf("workspace search index: nil index")
	}
	if ref.Path == "" {
		return fmt.Errorf("workspace search index: empty path")
	}

	terms := corpusTerms(ref.Path, data)

	i.mu.Lock()
	defer i.mu.Unlock()

	doc, ok := i.docs[ref.Path]
	if !ok {
		i.nextID++
		doc = &trigramDoc{id: i.nextID}
		i.docs[ref.Path] = doc
		i.byID[doc.id] = doc
	}
	if doc.ref.Digest == ref.Digest && doc.ref.Size == ref.Size && doc.ref.Path == ref.Path {
		return nil
	}

	oldTerms := doc.terms
	oldSet := make(map[string]struct{}, len(oldTerms))
	for _, term := range oldTerms {
		oldSet[term] = struct{}{}
	}
	newSet := make(map[string]struct{}, len(terms))
	for _, term := range terms {
		newSet[term] = struct{}{}
	}

	for term := range oldSet {
		if _, ok := newSet[term]; ok {
			continue
		}
		i.postings[term] = removeDocID(i.postings[term], doc.id)
		if len(i.postings[term]) == 0 {
			delete(i.postings, term)
		}
	}
	for term := range newSet {
		if _, ok := oldSet[term]; ok {
			continue
		}
		i.postings[term] = insertDocID(i.postings[term], doc.id)
	}

	doc.ref = ref
	doc.terms = terms
	return nil
}

// Delete removes a workspace path from the search index.
func (i *TrigramIndex) Delete(ctx context.Context, path string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if i == nil {
		return fmt.Errorf("workspace search index: nil index")
	}
	if path == "" {
		return nil
	}

	i.mu.Lock()
	defer i.mu.Unlock()

	doc, ok := i.docs[path]
	if !ok {
		return nil
	}
	for _, term := range doc.terms {
		i.postings[term] = removeDocID(i.postings[term], doc.id)
		if len(i.postings[term]) == 0 {
			delete(i.postings, term)
		}
	}
	delete(i.docs, path)
	delete(i.byID, doc.id)
	return nil
}
