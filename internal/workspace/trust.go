package workspace

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

type TrustStore struct {
	path string
}

type TrustState struct {
	TrustedAt time.Time `json:"trusted_at"`
}

type trustFile struct {
	Workspaces map[string]TrustState `json:"workspaces"`
}

func DefaultTrustPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "trusted_workspaces.json"), nil
}

func NewTrustStore(path string) *TrustStore {
	return &TrustStore{path: path}
}

func (s *TrustStore) IsTrusted(path string) (bool, error) {
	root, err := normalizePath(path)
	if err != nil {
		return false, err
	}
	data, err := s.load()
	if err != nil {
		return false, err
	}
	_, ok := data.Workspaces[root]
	return ok, nil
}

func (s *TrustStore) Trust(path string) error {
	root, err := normalizePath(path)
	if err != nil {
		return err
	}
	data, err := s.load()
	if err != nil {
		return err
	}
	if data.Workspaces == nil {
		data.Workspaces = map[string]TrustState{}
	}
	data.Workspaces[root] = TrustState{TrustedAt: time.Now().UTC()}
	return s.save(data)
}

func (s *TrustStore) List() ([]string, error) {
	data, err := s.load()
	if err != nil {
		return nil, err
	}
	paths := make([]string, 0, len(data.Workspaces))
	for path := range data.Workspaces {
		paths = append(paths, path)
	}
	sort.Strings(paths)
	return paths, nil
}

func (s *TrustStore) load() (trustFile, error) {
	if strings.TrimSpace(s.path) == "" {
		return trustFile{Workspaces: map[string]TrustState{}}, nil
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			return trustFile{Workspaces: map[string]TrustState{}}, nil
		}
		return trustFile{}, err
	}
	var file trustFile
	if err := json.Unmarshal(data, &file); err != nil {
		return trustFile{}, fmt.Errorf("parse trusted workspaces: %w", err)
	}
	if file.Workspaces == nil {
		file.Workspaces = map[string]TrustState{}
	}
	return file, nil
}

func (s *TrustStore) save(data trustFile) error {
	if strings.TrimSpace(s.path) == "" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(s.path), 0o755); err != nil {
		return err
	}
	encoded, err := json.MarshalIndent(data, "", "  ")
	if err != nil {
		return err
	}
	encoded = append(encoded, '\n')
	return os.WriteFile(s.path, encoded, 0o600)
}

func normalizePath(path string) (string, error) {
	if strings.TrimSpace(path) == "" {
		return "", fmt.Errorf("workspace path is required")
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.Clean(abs), nil
}
