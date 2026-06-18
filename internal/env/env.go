// Package env provides execution environment abstractions for file system
// and shell operations. This enables testability and platform abstraction.
package env

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// FileError represents a file system operation error.
type FileError struct {
	Code string
	Path string
	Err  error
}

func (e *FileError) Error() string {
	return fmt.Sprintf("file error (%s): %s: %v", e.Code, e.Path, e.Err)
}

func (e *FileError) Unwrap() error {
	return e.Err
}

// ExecutionError represents a shell execution error.
type ExecutionError struct {
	Code string
	Err  error
}

func (e *ExecutionError) Error() string {
	return fmt.Sprintf("execution error (%s): %v", e.Code, e.Err)
}

func (e *ExecutionError) Unwrap() error {
	return e.Err
}

// FileSystem provides file system operations.
type FileSystem interface {
	// ReadFile reads the named file and returns its contents.
	ReadFile(path string) ([]byte, error)

	// WriteFile writes data to the named file, creating it if necessary.
	WriteFile(path string, data []byte, perm os.FileMode) error

	// MkdirAll creates a directory named path, along with any necessary parents.
	MkdirAll(path string, perm os.FileMode) error

	// Remove removes the named file or directory.
	Remove(path string) error

	// RemoveAll removes path and any children it contains.
	RemoveAll(path string) error

	// Exists reports whether the named file or directory exists.
	Exists(path string) (bool, error)

	// ReadDir reads the named directory, returning all its directory entries.
	ReadDir(path string) ([]os.DirEntry, error)

	// Stat returns a FileInfo describing the named file.
	Stat(path string) (os.FileInfo, error)

	// Join joins any number of path elements into a single path.
	Join(elem ...string) string

	// Abs returns an absolute representation of path.
	Abs(path string) (string, error)

	// Glob returns the names of all files matching pattern.
	Glob(pattern string) ([]string, error)
}

// ShellExecutor provides shell command execution.
type ShellExecutor interface {
	// Exec executes a shell command and returns its output.
	Exec(ctx context.Context, command string, opts *ExecOptions) (*ExecResult, error)
}

// ExecOptions contains options for shell command execution.
type ExecOptions struct {
	// Cwd is the working directory for the command.
	Cwd string

	// Env contains additional environment variables.
	Env map[string]string

	// Timeout is the maximum duration for the command.
	TimeoutSeconds int

	// OnStdout is called with each chunk of stdout output.
	OnStdout func(chunk string)

	// OnStderr is called with each chunk of stderr output.
	OnStderr func(chunk string)
}

// ExecResult contains the result of a shell command execution.
type ExecResult struct {
	Stdout   string
	Stderr   string
	ExitCode int
}

// DefaultFileSystem implements FileSystem using the os package.
type DefaultFileSystem struct {
	BaseDir string
}

func NewDefaultFileSystem(baseDir string) *DefaultFileSystem {
	return &DefaultFileSystem{BaseDir: baseDir}
}

func (fs *DefaultFileSystem) resolve(path string) string {
	if filepath.IsAbs(path) {
		return path
	}
	return filepath.Join(fs.BaseDir, path)
}

func (fs *DefaultFileSystem) ReadFile(path string) ([]byte, error) {
	data, err := os.ReadFile(fs.resolve(path))
	if err != nil {
		return nil, &FileError{Code: "read_error", Path: path, Err: err}
	}
	return data, nil
}

func (fs *DefaultFileSystem) WriteFile(path string, data []byte, perm os.FileMode) error {
	dir := filepath.Dir(fs.resolve(path))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return &FileError{Code: "mkdir_error", Path: path, Err: err}
	}
	if err := os.WriteFile(fs.resolve(path), data, perm); err != nil {
		return &FileError{Code: "write_error", Path: path, Err: err}
	}
	return nil
}

func (fs *DefaultFileSystem) MkdirAll(path string, perm os.FileMode) error {
	if err := os.MkdirAll(fs.resolve(path), perm); err != nil {
		return &FileError{Code: "mkdir_error", Path: path, Err: err}
	}
	return nil
}

func (fs *DefaultFileSystem) Remove(path string) error {
	if err := os.Remove(fs.resolve(path)); err != nil {
		return &FileError{Code: "remove_error", Path: path, Err: err}
	}
	return nil
}

func (fs *DefaultFileSystem) RemoveAll(path string) error {
	if err := os.RemoveAll(fs.resolve(path)); err != nil {
		return &FileError{Code: "remove_error", Path: path, Err: err}
	}
	return nil
}

func (fs *DefaultFileSystem) Exists(path string) (bool, error) {
	_, err := os.Stat(fs.resolve(path))
	if err == nil {
		return true, nil
	}
	if os.IsNotExist(err) {
		return false, nil
	}
	return false, &FileError{Code: "stat_error", Path: path, Err: err}
}

func (fs *DefaultFileSystem) ReadDir(path string) ([]os.DirEntry, error) {
	entries, err := os.ReadDir(fs.resolve(path))
	if err != nil {
		return nil, &FileError{Code: "readdir_error", Path: path, Err: err}
	}
	return entries, nil
}

func (fs *DefaultFileSystem) Stat(path string) (os.FileInfo, error) {
	info, err := os.Stat(fs.resolve(path))
	if err != nil {
		return nil, &FileError{Code: "stat_error", Path: path, Err: err}
	}
	return info, nil
}

func (fs *DefaultFileSystem) Join(elem ...string) string {
	return filepath.Join(elem...)
}

func (fs *DefaultFileSystem) Abs(path string) (string, error) {
	if filepath.IsAbs(path) {
		return path, nil
	}
	abs, err := filepath.Abs(filepath.Join(fs.BaseDir, path))
	if err != nil {
		return "", &FileError{Code: "abs_error", Path: path, Err: err}
	}
	return abs, nil
}

func (fs *DefaultFileSystem) Glob(pattern string) ([]string, error) {
	matches, err := filepath.Glob(pattern)
	if err != nil {
		return nil, &FileError{Code: "glob_error", Path: pattern, Err: err}
	}
	return matches, nil
}

// DefaultShellExecutor implements ShellExecutor using os/exec.
type DefaultShellExecutor struct {
	ShellPath string
	BaseEnv   map[string]string
}

func NewDefaultShellExecutor(shellPath string, baseEnv map[string]string) *DefaultShellExecutor {
	if shellPath == "" {
		shellPath = defaultShell()
	}
	return &DefaultShellExecutor{
		ShellPath: shellPath,
		BaseEnv:   baseEnv,
	}
}

func defaultShell() string {
	if runtime.GOOS == "windows" {
		if sh := os.Getenv("COMSPEC"); sh != "" {
			return sh
		}
		return "cmd.exe"
	}
	if sh := os.Getenv("SHELL"); sh != "" {
		return sh
	}
	return "/bin/sh"
}

func (se *DefaultShellExecutor) Exec(ctx context.Context, command string, opts *ExecOptions) (*ExecResult, error) {
	if opts == nil {
		opts = &ExecOptions{}
	}

	args := shellArgs(se.ShellPath, command)
	cmd := exec.CommandContext(ctx, se.ShellPath, args...)

	if opts.Cwd != "" {
		cmd.Dir = opts.Cwd
	}

	// Merge environment variables
	cmd.Env = os.Environ()
	for k, v := range se.BaseEnv {
		cmd.Env = append(cmd.Env, k+"="+v)
	}
	for k, v := range opts.Env {
		cmd.Env = append(cmd.Env, k+"="+v)
	}

	var stdout, stderr strings.Builder
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if opts.OnStdout != nil {
		cmd.Stdout = &writerFunc{fn: opts.OnStdout}
	}
	if opts.OnStderr != nil {
		cmd.Stderr = &writerFunc{fn: opts.OnStderr}
	}

	err := cmd.Run()
	exitCode := 0
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			exitCode = exitErr.ExitCode()
		} else {
			return nil, &ExecutionError{Code: "spawn_error", Err: err}
		}
	}

	return &ExecResult{
		Stdout:   stdout.String(),
		Stderr:   stderr.String(),
		ExitCode: exitCode,
	}, nil
}

func shellArgs(shell, command string) []string {
	if runtime.GOOS == "windows" {
		return []string{"/c", command}
	}
	return []string{"-c", command}
}

// writerFunc adapts a function to io.Writer.
type writerFunc struct {
	fn func(string)
}

func (w *writerFunc) Write(p []byte) (n int, err error) {
	w.fn(string(p))
	return len(p), nil
}
