package workspace

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"time"
)

const checkpointManifestName = "manifest.json"

type CheckpointStore struct {
	path string
}

type Checkpoint struct {
	ID        string            `json:"id"`
	Workspace string            `json:"workspace"`
	CreatedAt time.Time         `json:"created_at"`
	Entries   []CheckpointEntry `json:"entries"`
}

type CheckpointEntry struct {
	Path   string      `json:"path"`
	State  PathState   `json:"state"`
	Mode   os.FileMode `json:"mode,omitempty"`
	Size   int64       `json:"size,omitempty"`
	Digest string      `json:"digest,omitempty"`
	Blob   string      `json:"blob,omitempty"`
}

type PathState string

const (
	PathAbsent PathState = "absent"
	PathFile   PathState = "file"
	PathDir    PathState = "dir"
)

type RestoreReport struct {
	Restored []string
	Removed  []string
}

func DefaultCheckpointPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "checkpoints"), nil
}

func NewCheckpointStore(path string) *CheckpointStore {
	return &CheckpointStore{path: path}
}

func (s *CheckpointStore) Create(ctx context.Context, workspacePath string, paths []string) (Checkpoint, error) {
	if err := ctx.Err(); err != nil {
		return Checkpoint{}, err
	}
	if strings.TrimSpace(s.path) == "" {
		return Checkpoint{}, fmt.Errorf("checkpoint store path is required")
	}
	rootPath, err := normalizePath(workspacePath)
	if err != nil {
		return Checkpoint{}, err
	}
	root, err := os.OpenRoot(rootPath)
	if err != nil {
		return Checkpoint{}, fmt.Errorf("open workspace: %w", err)
	}
	defer root.Close()

	id, err := newCheckpointID()
	if err != nil {
		return Checkpoint{}, err
	}
	checkpointPath := filepath.Join(s.path, id)
	blobPath := filepath.Join(checkpointPath, "blobs")
	if err := os.MkdirAll(blobPath, 0o700); err != nil {
		return Checkpoint{}, err
	}

	normalized, err := normalizeCheckpointPaths(paths)
	if err != nil {
		return Checkpoint{}, err
	}
	entries := make([]CheckpointEntry, 0, len(normalized))
	for _, path := range normalized {
		entry, err := snapshotPath(ctx, root, path, blobPath)
		if err != nil {
			return Checkpoint{}, err
		}
		entries = append(entries, entry)
	}

	cp := Checkpoint{
		ID:        id,
		Workspace: rootPath,
		CreatedAt: time.Now().UTC(),
		Entries:   entries,
	}
	if err := writeManifest(checkpointPath, cp); err != nil {
		return Checkpoint{}, err
	}
	return cp, nil
}

func (s *CheckpointStore) Load(id string) (Checkpoint, error) {
	id = strings.TrimSpace(id)
	if id == "" || strings.Contains(id, "/") || strings.Contains(id, `\`) {
		return Checkpoint{}, fmt.Errorf("checkpoint id is invalid")
	}
	data, err := os.ReadFile(filepath.Join(s.path, id, checkpointManifestName))
	if err != nil {
		return Checkpoint{}, err
	}
	var cp Checkpoint
	if err := json.Unmarshal(data, &cp); err != nil {
		return Checkpoint{}, fmt.Errorf("parse checkpoint manifest: %w", err)
	}
	if cp.ID != id {
		return Checkpoint{}, fmt.Errorf("checkpoint manifest id mismatch")
	}
	return cp, nil
}

func (s *CheckpointStore) Restore(ctx context.Context, cp Checkpoint) (RestoreReport, error) {
	if err := ctx.Err(); err != nil {
		return RestoreReport{}, err
	}
	if strings.TrimSpace(cp.ID) == "" {
		return RestoreReport{}, fmt.Errorf("checkpoint id is required")
	}
	rootPath, err := normalizePath(cp.Workspace)
	if err != nil {
		return RestoreReport{}, err
	}
	root, err := os.OpenRoot(rootPath)
	if err != nil {
		return RestoreReport{}, fmt.Errorf("open workspace: %w", err)
	}
	defer root.Close()

	report := RestoreReport{}
	for _, entry := range cp.Entries {
		if err := ctx.Err(); err != nil {
			return report, err
		}
		path, err := normalizeCheckpointPath(entry.Path)
		if err != nil {
			return report, err
		}
		switch entry.State {
		case PathAbsent:
			if err := root.RemoveAll(path); err != nil && !os.IsNotExist(err) {
				return report, fmt.Errorf("remove %s: %w", path, err)
			}
			report.Removed = append(report.Removed, path)
		case PathDir:
			if err := root.MkdirAll(path, entry.Mode.Perm()); err != nil {
				return report, fmt.Errorf("restore dir %s: %w", path, err)
			}
			report.Restored = append(report.Restored, path)
		case PathFile:
			data, err := os.ReadFile(filepath.Join(s.path, cp.ID, "blobs", entry.Blob))
			if err != nil {
				return report, fmt.Errorf("read checkpoint blob %s: %w", entry.Blob, err)
			}
			if got := digest(data); got != entry.Digest {
				return report, fmt.Errorf("checkpoint blob digest mismatch for %s", path)
			}
			if dir := filepath.Dir(path); dir != "." {
				if err := root.MkdirAll(dir, 0o755); err != nil {
					return report, fmt.Errorf("restore parent %s: %w", dir, err)
				}
			}
			if err := root.WriteFile(path, data, entry.Mode.Perm()); err != nil {
				return report, fmt.Errorf("restore file %s: %w", path, err)
			}
			report.Restored = append(report.Restored, path)
		default:
			return report, fmt.Errorf("unknown checkpoint state %q for %s", entry.State, path)
		}
	}
	return report, nil
}

func snapshotPath(ctx context.Context, root *os.Root, path, blobPath string) (CheckpointEntry, error) {
	if err := ctx.Err(); err != nil {
		return CheckpointEntry{}, err
	}
	info, err := root.Stat(path)
	if err != nil {
		if os.IsNotExist(err) {
			return CheckpointEntry{Path: path, State: PathAbsent}, nil
		}
		return CheckpointEntry{}, fmt.Errorf("stat %s: %w", path, err)
	}
	if info.IsDir() {
		return CheckpointEntry{Path: path, State: PathDir, Mode: info.Mode()}, nil
	}
	if !info.Mode().IsRegular() {
		return CheckpointEntry{}, fmt.Errorf("checkpoint %s: unsupported file mode %s", path, info.Mode())
	}
	data, err := root.ReadFile(path)
	if err != nil {
		return CheckpointEntry{}, fmt.Errorf("read %s: %w", path, err)
	}
	sum := digest(data)
	blobName := strings.TrimPrefix(sum, "sha256:")
	if err := os.WriteFile(filepath.Join(blobPath, blobName), data, 0o600); err != nil {
		return CheckpointEntry{}, fmt.Errorf("write checkpoint blob: %w", err)
	}
	return CheckpointEntry{
		Path:   path,
		State:  PathFile,
		Mode:   info.Mode(),
		Size:   int64(len(data)),
		Digest: sum,
		Blob:   blobName,
	}, nil
}

func writeManifest(path string, cp Checkpoint) error {
	data, err := json.MarshalIndent(cp, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	return os.WriteFile(filepath.Join(path, checkpointManifestName), data, 0o600)
}

func normalizeCheckpointPaths(paths []string) ([]string, error) {
	seen := map[string]struct{}{}
	normalized := make([]string, 0, len(paths))
	for _, path := range paths {
		clean, err := normalizeCheckpointPath(path)
		if err != nil {
			return nil, err
		}
		if _, ok := seen[clean]; ok {
			continue
		}
		seen[clean] = struct{}{}
		normalized = append(normalized, clean)
	}
	slices.Sort(normalized)
	return normalized, nil
}

func normalizeCheckpointPath(name string) (string, error) {
	name = filepath.ToSlash(strings.TrimSpace(name))
	if name == "" || name == "." {
		return "", fmt.Errorf("checkpoint path is required")
	}
	if filepath.IsAbs(name) || strings.HasPrefix(name, "../") || strings.Contains(name, "/../") || name == ".." {
		return "", fmt.Errorf("checkpoint path escapes workspace: %s", name)
	}
	return path.Clean(name), nil
}

func newCheckpointID() (string, error) {
	var b [8]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	return fmt.Sprintf("%d-%s", time.Now().UTC().UnixNano(), hex.EncodeToString(b[:])), nil
}

func digest(data []byte) string {
	sum := sha256.Sum256(data)
	return "sha256:" + hex.EncodeToString(sum[:])
}
