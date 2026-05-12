package workspace

import (
	"fmt"
	"path/filepath"
	"strings"
)

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
