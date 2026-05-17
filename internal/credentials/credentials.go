package credentials

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

type Provider struct {
	APIKey string `toml:"api_key,omitempty"`
}

type File struct {
	Providers map[string]Provider `toml:"providers,omitempty"`
}

func DefaultPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "credentials.toml"), nil
}

func LookupAPIKey(provider string) (string, bool) {
	file, err := Load()
	if err != nil {
		return "", false
	}
	credential, ok := file.Providers[normalizeProvider(provider)]
	if !ok {
		return "", false
	}
	key := strings.TrimSpace(credential.APIKey)
	return key, key != ""
}

func SaveAPIKey(provider, key string) error {
	provider = normalizeProvider(provider)
	key = strings.TrimSpace(key)
	if provider == "" {
		return fmt.Errorf("provider is required")
	}
	if key == "" {
		return fmt.Errorf("API key cannot be empty")
	}

	file, err := Load()
	if err != nil {
		return err
	}
	if file.Providers == nil {
		file.Providers = map[string]Provider{}
	}
	file.Providers[provider] = Provider{APIKey: key}
	return Save(file)
}

func Load() (*File, error) {
	path, err := DefaultPath()
	if err != nil {
		return nil, err
	}
	file := &File{Providers: map[string]Provider{}}
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return file, nil
		}
		return nil, err
	}
	if err := toml.Unmarshal(data, file); err != nil {
		return nil, fmt.Errorf("failed to parse credentials: %w", err)
	}
	if file.Providers == nil {
		file.Providers = map[string]Provider{}
	}
	return file, nil
}

func Save(file *File) error {
	if file == nil {
		file = &File{}
	}
	if file.Providers == nil {
		file.Providers = map[string]Provider{}
	}
	path, err := DefaultPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	data, err := toml.Marshal(file)
	if err != nil {
		return err
	}
	return writeFileAtomic(path, data, 0o600)
}

func writeFileAtomic(path string, data []byte, perm os.FileMode) error {
	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, "."+filepath.Base(path)+".tmp-*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer func() {
		_ = os.Remove(tmpName)
	}()

	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Chmod(perm); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}

func normalizeProvider(provider string) string {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "local-api", "custom-api":
		return "openai-compatible"
	default:
		return strings.ToLower(strings.TrimSpace(provider))
	}
}
