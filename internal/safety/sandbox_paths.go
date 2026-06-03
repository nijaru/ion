package safety

import (
	"path/filepath"
	"slices"
)

func uniquePaths(paths []string) []string {
	if len(paths) == 0 {
		return nil
	}
	cleaned := make([]string, 0, len(paths))
	for _, path := range paths {
		if path == "" {
			continue
		}
		cleaned = append(cleaned, filepath.Clean(path))
	}
	slices.Sort(cleaned)
	return slices.Compact(cleaned)
}
