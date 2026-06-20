package tool

import (
	"context"
	"io"
	"os"
	"os/exec"
)

// Operations abstracts filesystem and process execution for testability.
// The default implementation (LocalOperations) uses the real OS.
type Operations interface {
	// ReadFile reads a file from the filesystem.
	ReadFile(name string) ([]byte, error)

	// Stat returns a FileInfo describing the named file.
	Stat(name string) (os.FileInfo, error)

	// MkdirAll creates a directory path and all parents that do not exist.
	MkdirAll(path string, perm os.FileMode) error

	// Rename renames (moves) a file.
	Rename(oldpath, newpath string) error

	// WriteFile writes data to a file, creating it if needed.
	WriteFile(name string, data []byte, perm os.FileMode) error

	// CreateTemp creates a new temporary file in the directory.
	CreateTemp(dir, pattern string) (*os.File, error)

	// Remove removes the named file or directory.
	Remove(name string) error

	// CommandContext creates a new Cmd with a context.
	CommandContext(ctx context.Context, name string, arg ...string) *exec.Cmd

	// LookPath searches for an executable named file in PATH.
	LookPath(file string) (string, error)

	// Open opens a file for reading.
	Open(name string) (*os.File, error)
}

// LocalOperations implements Operations using the real OS filesystem and exec.
type LocalOperations struct{}

func (LocalOperations) ReadFile(name string) ([]byte, error)                { return os.ReadFile(name) }
func (LocalOperations) Stat(name string) (os.FileInfo, error)               { return os.Stat(name) }
func (LocalOperations) MkdirAll(path string, perm os.FileMode) error       { return os.MkdirAll(path, perm) }
func (LocalOperations) Rename(oldpath, newpath string) error                { return os.Rename(oldpath, newpath) }
func (LocalOperations) WriteFile(name string, data []byte, perm os.FileMode) error {
	return os.WriteFile(name, data, perm)
}
func (LocalOperations) CreateTemp(dir, pattern string) (*os.File, error) { return os.CreateTemp(dir, pattern) }
func (LocalOperations) Remove(name string) error                        { return os.Remove(name) }
func (LocalOperations) CommandContext(ctx context.Context, name string, arg ...string) *exec.Cmd {
	return exec.CommandContext(ctx, name, arg...)
}
func (LocalOperations) LookPath(file string) (string, error) { return exec.LookPath(file) }
func (LocalOperations) Open(name string) (*os.File, error)   { return os.Open(name) }

// NopOperations is a no-op implementation for testing.
// All methods return zero values or nil errors.
type NopOperations struct {
	FileContents map[string][]byte
	Files        map[string]os.FileInfo
	Commands     []*exec.Cmd
}

func (n *NopOperations) ReadFile(name string) ([]byte, error) {
	if n.FileContents != nil {
		if data, ok := n.FileContents[name]; ok {
			return data, nil
		}
	}
	return nil, os.ErrNotExist
}

func (n *NopOperations) Stat(name string) (os.FileInfo, error) {
	if n.Files != nil {
		if info, ok := n.Files[name]; ok {
			return info, nil
		}
	}
	return nil, os.ErrNotExist
}

func (n *NopOperations) MkdirAll(path string, perm os.FileMode) error { return nil }
func (n *NopOperations) Rename(oldpath, newpath string) error          { return nil }
func (n *NopOperations) WriteFile(name string, data []byte, perm os.FileMode) error {
	if n.FileContents == nil {
		n.FileContents = make(map[string][]byte)
	}
	n.FileContents[name] = data
	return nil
}

func (n *NopOperations) CreateTemp(dir, pattern string) (*os.File, error) {
	return os.CreateTemp(dir, pattern)
}

func (n *NopOperations) Remove(name string) error { return nil }

func (n *NopOperations) CommandContext(ctx context.Context, name string, arg ...string) *exec.Cmd {
	cmd := exec.CommandContext(ctx, "true") // no-op command
	n.Commands = append(n.Commands, cmd)
	return cmd
}

func (n *NopOperations) LookPath(file string) (string, error) {
	return "/usr/bin/" + file, nil
}

func (n *NopOperations) Open(name string) (*os.File, error) {
	return nil, os.ErrNotExist
}

// ReaderOperations wraps an io.Reader for testing read operations.
type ReaderOperations struct {
	LocalOperations
	Reader io.Reader
}

func (r *ReaderOperations) Open(name string) (*os.File, error) {
	// For testing, we need to return a real *os.File
	// This is a limitation — we can't easily mock *os.File
	return os.Open(name)
}
