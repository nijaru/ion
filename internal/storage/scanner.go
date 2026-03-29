package storage

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"time"

	ignore "github.com/sabhiram/go-gitignore"
)

// Scanner handles workspace crawling for background indexing.
type Scanner struct {
	CWD   string
	Store Store
}

// FileInfo represents a file found during a scan.
type FileInfo struct {
	Path      string
	Hash      string
	Size      int64
	UpdatedAt time.Time
}

func NewScanner(cwd string, store Store) *Scanner {
	return &Scanner{
		CWD:   cwd,
		Store: store,
	}
}

// Scan workspace and call fn for each file found.
func (s *Scanner) Scan(ctx context.Context, fn func(FileInfo) error) error {
	ignorer := s.loadGitignore()

	return filepath.WalkDir(s.CWD, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		rel, _ := filepath.Rel(s.CWD, path)
		if rel == "." {
			return nil
		}

		if ignorer != nil && ignorer.MatchesPath(rel) {
			if d.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}

		if d.IsDir() {
			if d.Name() == ".git" {
				return filepath.SkipDir
			}
			return nil
		}

		info, err := d.Info()
		if err != nil {
			return nil
		}

		// Calculate hash for change detection
		hash, _ := s.hashFile(path)
		
		return fn(FileInfo{
			Path:      rel,
			Hash:      hash,
			Size:      info.Size(),
			UpdatedAt: info.ModTime(),
		})
	})
}

func (s *Scanner) loadGitignore() *ignore.GitIgnore {
	data, err := os.ReadFile(filepath.Join(s.CWD, ".gitignore"))
	if err != nil {
		return nil
	}
	return ignore.CompileIgnoreLines(filepath.Join(s.CWD, ".gitignore"), string(data))
}

func (s *Scanner) hashFile(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()

	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}

	return hex.EncodeToString(h.Sum(nil)), nil
}
