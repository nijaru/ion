package tool

import (
	"path/filepath"
	"sync"
)

// fileMutationQueue serializes file mutation operations targeting the same file.
// Operations for different files run in parallel.
type fileMutationQueue struct {
	mu     sync.Mutex
	queues map[string]chan struct{}
}

var globalFileQueue = &fileMutationQueue{
	queues: make(map[string]chan struct{}),
}

// getQueueKey returns the canonical key for a file path (resolved via realpath).
func getQueueKey(filePath string) string {
	resolved, err := filepath.Abs(filePath)
	if err != nil {
		return filePath
	}
	// Try to resolve symlinks for consistent keying
	if real, err := filepath.EvalSymlinks(resolved); err == nil {
		return real
	}
	return resolved
}

// WithFileMutationQueue serializes fn against other mutations to the same file.
// Operations for different files run in parallel.
func WithFileMutationQueue(filePath string, fn func() (string, error)) (string, error) {
	key := getQueueKey(filePath)

	globalFileQueue.mu.Lock()
	ch, exists := globalFileQueue.queues[key]
	if !exists {
		ch = make(chan struct{}, 1)
		globalFileQueue.queues[key] = ch
	}
	globalFileQueue.mu.Unlock()

	// Acquire the per-file lock
	ch <- struct{}{}
	defer func() {
		<-ch
		// Clean up if no one is waiting
		globalFileQueue.mu.Lock()
		if len(ch) == 0 {
			delete(globalFileQueue.queues, key)
		}
		globalFileQueue.mu.Unlock()
	}()

	return fn()
}
