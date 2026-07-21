package agent

import (
	"path/filepath"
	"slices"
	"sync"
)

// actionPathLocks serializes mutating actions within one runtime. The action
// coordinator owns this state; file tools do not maintain a process-global
// mutation queue or make concurrency decisions themselves.
type actionPathLocks struct {
	mu      sync.Mutex
	entries map[string]*actionPathLockEntry
}

type actionPathLockEntry struct {
	mu   sync.Mutex
	refs int
}

func newActionPathLocks() *actionPathLocks {
	return &actionPathLocks{entries: make(map[string]*actionPathLockEntry)}
}

// acquire locks all paths in canonical order and returns a release function.
// Sorting is required so overlapping multi-file actions cannot deadlock.
func (l *actionPathLocks) acquire(paths []string) func() {
	if l == nil || len(paths) == 0 {
		return func() {}
	}
	keys := make([]string, 0, len(paths))
	for _, path := range paths {
		path = filepath.Clean(path)
		if path != "" {
			keys = append(keys, path)
		}
	}
	slices.Sort(keys)
	keys = slices.Compact(keys)
	if len(keys) == 0 {
		return func() {}
	}

	entries := make([]*actionPathLockEntry, len(keys))
	l.mu.Lock()
	for i, key := range keys {
		entry := l.entries[key]
		if entry == nil {
			entry = &actionPathLockEntry{}
			l.entries[key] = entry
		}
		entry.refs++
		entries[i] = entry
	}
	l.mu.Unlock()

	for _, entry := range entries {
		entry.mu.Lock()
	}
	return func() {
		for i := len(entries) - 1; i >= 0; i-- {
			entries[i].mu.Unlock()
		}
		l.mu.Lock()
		for i, key := range keys {
			entry := entries[i]
			entry.refs--
			if entry.refs == 0 {
				delete(l.entries, key)
			}
		}
		l.mu.Unlock()
	}
}
