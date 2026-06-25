// Package gitwatch provides real-time git branch change detection.
// It watches .git/HEAD for changes and notifies subscribers.
package gitwatch

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// Watcher monitors the current git branch and notifies on changes.
type Watcher struct {
	cwd        string
	gitDir     string // .git directory path
	headPath   string // .git/HEAD path
	cachedBranch string
	mu         sync.Mutex
	callbacks  []func(string)
	done       chan struct{}
	interval   time.Duration
}

// New creates a new git branch watcher for the given working directory.
// Returns nil if not in a git repository.
func New(cwd string) *Watcher {
	gitDir, headPath := findGitPaths(cwd)
	if gitDir == "" {
		return nil
	}
	w := &Watcher{
		cwd:      cwd,
		gitDir:   gitDir,
		headPath: headPath,
		done:     make(chan struct{}),
		interval: 2 * time.Second,
	}
	w.cachedBranch = w.readBranch()
	return w
}

// Branch returns the current git branch, or empty string if not in a repo.
func (w *Watcher) Branch() string {
	if w == nil {
		return ""
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.cachedBranch
}

// OnChange registers a callback that fires when the branch changes.
func (w *Watcher) OnChange(fn func(string)) {
	if w == nil {
		return
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	w.callbacks = append(w.callbacks, fn)
}

// Start begins background polling for branch changes.
func (w *Watcher) Start() {
	if w == nil {
		return
	}
	go w.poll()
}

// Stop stops the watcher.
func (w *Watcher) Stop() {
	if w == nil {
		return
	}
	select {
	case <-w.done:
	default:
		close(w.done)
	}
}

func (w *Watcher) poll() {
	ticker := time.NewTicker(w.interval)
	defer ticker.Stop()
	for {
		select {
		case <-w.done:
			return
		case <-ticker.C:
			w.check()
		}
	}
}

func (w *Watcher) check() {
	branch := w.readBranch()
	w.mu.Lock()
	if branch != w.cachedBranch {
		w.cachedBranch = branch
		callbacks := make([]func(string), len(w.callbacks))
		copy(callbacks, w.callbacks)
		w.mu.Unlock()
		for _, fn := range callbacks {
			fn(branch)
		}
		return
	}
	w.mu.Unlock()
}

func (w *Watcher) readBranch() string {
	data, err := os.ReadFile(w.headPath)
	if err != nil {
		return ""
	}
	content := strings.TrimSpace(string(data))
	if strings.HasPrefix(content, "ref: refs/heads/") {
		return strings.TrimPrefix(content, "ref: refs/heads/")
	}
	// Detached HEAD — try git command
	return ""
}

// findGitPaths locates the .git directory and HEAD file.
// Handles both regular repos (.git is directory) and worktrees (.git is file).
func findGitPaths(cwd string) (gitDir, headPath string) {
	dir := cwd
	for {
		gitPath := filepath.Join(dir, ".git")
		info, err := os.Stat(gitPath)
		if err != nil {
			parent := filepath.Dir(dir)
			if parent == dir {
				return "", ""
			}
			dir = parent
			continue
		}
		if info.IsDir() {
			head := filepath.Join(gitPath, "HEAD")
			if _, err := os.Stat(head); err != nil {
				return "", ""
			}
			return gitPath, head
		}
		// .git is a file (worktree) — read gitdir path
		data, err := os.ReadFile(gitPath)
		if err != nil {
			return "", ""
		}
		content := strings.TrimSpace(string(data))
		if !strings.HasPrefix(content, "gitdir: ") {
			return "", ""
		}
		gitDirPath := strings.TrimPrefix(content, "gitdir: ")
		if !filepath.IsAbs(gitDirPath) {
			gitDirPath = filepath.Join(dir, gitDirPath)
		}
		head := filepath.Join(gitDirPath, "HEAD")
		if _, err := os.Stat(head); err != nil {
			return "", ""
		}
		return gitDirPath, head
	}
}
