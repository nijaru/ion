package tool

import (
	"context"
	"os"
	"os/exec"
)

// FileReader abstracts the file read needed by the read tool.
type FileReader interface {
	// ReadFile reads a file from the filesystem.
	ReadFile(name string) ([]byte, error)
}

// commandOperations abstracts process creation for the shell executor.
type commandOperations interface {
	// CommandContext creates a new Cmd with a context.
	CommandContext(ctx context.Context, name string, arg ...string) *exec.Cmd
}

// LocalOperations implements FileReader and commandOperations using the OS.
type LocalOperations struct{}

var _ FileReader = LocalOperations{}
var _ commandOperations = LocalOperations{}

func (LocalOperations) ReadFile(name string) ([]byte, error) { return os.ReadFile(name) }
func (LocalOperations) CommandContext(ctx context.Context, name string, arg ...string) *exec.Cmd {
	return exec.CommandContext(ctx, name, arg...)
}
