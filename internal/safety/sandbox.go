package safety

import (
	"errors"
	"fmt"
	"os/exec"
)

var ErrSandboxUnavailable = errors.New("sandbox backend unavailable")

// SandboxOptions describe the execution boundary for one command.
type SandboxOptions struct {
	WorkDir       string
	ReadablePaths []string
	WritablePaths []string
	Network       bool
}

// Sandbox wraps an exec.Cmd in an OS-level sandbox backend.
type Sandbox interface {
	Wrap(cmd *exec.Cmd, opts SandboxOptions) error
}

// NewSandbox returns the platform-specific sandbox wrapper when available.
func NewSandbox() Sandbox {
	return newPlatformSandbox()
}

type unsupportedSandbox struct {
	reason string
}

func (s unsupportedSandbox) Wrap(cmd *exec.Cmd, opts SandboxOptions) error {
	return fmt.Errorf("%w: %s", ErrSandboxUnavailable, s.reason)
}
