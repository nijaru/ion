package main

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"

	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	ionmemory "github.com/nijaru/ion/memory"
)

type tuiMemoryController struct {
	path  string
	scope string
}

func (c tuiMemoryController) open() (*ionmemory.Store, error) {
	if c.path == "" {
		return nil, fmt.Errorf("memory path is not configured")
	}
	return ionmemory.Open(c.path)
}

func (c tuiMemoryController) Search(
	ctx context.Context,
	query string,
	includeDeleted bool,
	limit int,
) ([]app.MemoryRecord, error) {
	store, err := c.open()
	if err != nil {
		return nil, err
	}
	var records []ionmemory.Record
	if query == "" && includeDeleted {
		records, err = store.List(ctx, c.scope, true, limit)
	} else {
		records, err = store.Search(ctx, c.scope, query, limit)
	}
	closeErr := store.Close()
	if err != nil {
		return nil, errors.Join(err, closeErr)
	}
	if closeErr != nil {
		return nil, closeErr
	}
	result := make([]app.MemoryRecord, 0, len(records))
	for _, record := range records {
		result = append(result, app.MemoryRecord{
			ID:        record.ID,
			Content:   record.Content,
			Tags:      record.Tags,
			CreatedAt: record.CreatedAt,
			Deleted:   record.DeletedAt != nil,
		})
	}
	return result, nil
}

func (c tuiMemoryController) Delete(ctx context.Context, id string) error {
	return c.transition(ctx, id, true)
}

func (c tuiMemoryController) Audit(ctx context.Context, limit int) ([]app.MemoryAuditRecord, error) {
	store, err := c.open()
	if err != nil {
		return nil, err
	}
	entries, err := store.Audit(ctx, c.scope, limit)
	closeErr := store.Close()
	if err != nil {
		return nil, errors.Join(err, closeErr)
	}
	if closeErr != nil {
		return nil, closeErr
	}
	result := make([]app.MemoryAuditRecord, 0, len(entries))
	for _, entry := range entries {
		result = append(result, app.MemoryAuditRecord{
			Sequence:  entry.Sequence,
			MemoryID:  entry.MemoryID,
			Operation: entry.Operation,
			Content:   entry.Content,
			Tags:      entry.Tags,
			At:        entry.At,
		})
	}
	return result, nil
}

func (c tuiMemoryController) Restore(ctx context.Context, id string) error {
	return c.transition(ctx, id, false)
}

func (c tuiMemoryController) transition(ctx context.Context, id string, deleted bool) error {
	store, err := c.open()
	if err != nil {
		return err
	}
	if deleted {
		_, err = store.Delete(ctx, c.scope, id)
	} else {
		_, err = store.Restore(ctx, c.scope, id)
	}
	return errors.Join(err, store.Close())
}

func defaultMemoryPath() (string, error) {
	dataDir, err := config.DefaultDataDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dataDir, "memory.db"), nil
}
