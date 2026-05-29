//go:build linux

package safety

import (
	"os/exec"
	"path/filepath"
)

type BubblewrapSandbox struct {
	Command string
}

func newPlatformSandbox() Sandbox {
	return &BubblewrapSandbox{Command: "bwrap"}
}

func (s *BubblewrapSandbox) Wrap(cmd *exec.Cmd, opts SandboxOptions) error {
	command := s.Command
	if command == "" {
		command = "bwrap"
	}
	if _, err := exec.LookPath(command); err != nil {
		return unsupportedSandbox{reason: err.Error()}.Wrap(cmd, opts)
	}

	args := []string{
		"--die-with-parent",
		"--new-session",
		"--unshare-pid",
		"--proc", "/proc",
		"--dev", "/dev",
	}
	if !opts.Network {
		args = append(args, "--unshare-net")
	}
	for _, path := range uniqueLinuxPaths(opts.ReadablePaths, opts.WorkDir) {
		args = append(args, "--ro-bind", path, path)
	}
	for _, path := range uniqueLinuxPaths(opts.WritablePaths, opts.WorkDir) {
		args = append(args, "--bind", path, path)
	}
	if opts.WorkDir != "" {
		args = append(args, "--chdir", filepath.Clean(opts.WorkDir))
	}
	args = append(args, cmd.Args...)
	cmd.Path = command
	cmd.Args = append([]string{command}, args...)
	return nil
}

func uniqueLinuxPaths(paths []string, workDir string) []string {
	if workDir != "" {
		paths = append(paths, workDir)
	}
	return uniquePaths(paths)
}
