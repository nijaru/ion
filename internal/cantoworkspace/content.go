package workspace

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"time"
)

// ContentRef identifies file content without forcing callers to keep the
// materialized bytes around.
type ContentRef struct {
	Path    string
	Digest  string
	Size    int64
	ModTime time.Time
}

// Identity returns the stable content identity, preferring the digest when it
// is available.
func (r ContentRef) Identity() string {
	if r.Digest != "" {
		return r.Digest
	}
	return r.Path
}

// String returns a compact human-readable reference.
func (r ContentRef) String() string {
	if r.Digest == "" {
		return r.Path
	}
	return fmt.Sprintf("%s@%s", r.Path, r.Digest)
}

// RefFile materializes path once, computes its identity, and returns both the
// stable reference and the loaded bytes.
func RefFile(ctx context.Context, fs WorkspaceFS, path string) (ContentRef, []byte, error) {
	if err := ctx.Err(); err != nil {
		return ContentRef{}, nil, err
	}
	if fs == nil {
		return ContentRef{}, nil, fmt.Errorf("content ref: nil workspace fs")
	}

	data, err := fs.ReadFile(path)
	if err != nil {
		return ContentRef{}, nil, err
	}
	info, err := fs.Stat(path)
	if err != nil {
		return ContentRef{}, nil, err
	}

	sum := sha256.Sum256(data)
	ref := ContentRef{
		Path:    path,
		Digest:  "sha256:" + hex.EncodeToString(sum[:]),
		Size:    int64(len(data)),
		ModTime: info.ModTime(),
	}
	return ref, data, nil
}
