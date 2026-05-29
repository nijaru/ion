package workspace

import (
	"context"
	"fmt"
	"path"
)

// IndexFile reads one workspace file, computes its content identity, and
// upserts it into the provided search index.
func IndexFile(
	ctx context.Context,
	fs WorkspaceFS,
	index SearchIndex,
	path string,
) (ContentRef, error) {
	if err := ctx.Err(); err != nil {
		return ContentRef{}, err
	}
	if fs == nil {
		return ContentRef{}, fmt.Errorf("workspace index: nil workspace fs")
	}
	if index == nil {
		return ContentRef{}, fmt.Errorf("workspace index: nil search index")
	}

	ref, data, err := RefFile(ctx, fs, path)
	if err != nil {
		return ContentRef{}, err
	}
	if err := index.Upsert(ctx, ref, data); err != nil {
		return ContentRef{}, err
	}
	return ref, nil
}

// IndexWorkspace walks the rooted workspace and indexes every regular file.
func IndexWorkspace(ctx context.Context, ws WorkspaceFS, index SearchIndex) (int, error) {
	if err := ctx.Err(); err != nil {
		return 0, err
	}
	if ws == nil {
		return 0, fmt.Errorf("workspace index: nil workspace fs")
	}
	if index == nil {
		return 0, fmt.Errorf("workspace index: nil search index")
	}

	var count int
	err := walkWorkspaceFiles(ctx, ws, ".", func(filePath string) error {
		if _, err := IndexFile(ctx, ws, index, filePath); err != nil {
			return err
		}
		count++
		return nil
	})
	if err != nil {
		return count, err
	}
	return count, nil
}

func walkWorkspaceFiles(
	ctx context.Context,
	ws WorkspaceFS,
	dir string,
	visit func(path string) error,
) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	entries, err := ws.ReadDir(dir)
	if err != nil {
		return err
	}
	for _, entry := range entries {
		if err := ctx.Err(); err != nil {
			return err
		}
		child := entry.Name()
		if dir != "." && dir != "" {
			child = path.Join(dir, child)
		}
		if entry.IsDir() {
			if err := walkWorkspaceFiles(ctx, ws, child, visit); err != nil {
				return err
			}
			continue
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			continue
		}
		if err := visit(child); err != nil {
			return err
		}
	}
	return nil
}
