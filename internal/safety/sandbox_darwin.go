//go:build darwin

package safety

import (
	"os/exec"
	"path/filepath"
	"strings"
)

type SeatbeltSandbox struct {
	Command string
}

func newPlatformSandbox() Sandbox {
	return &SeatbeltSandbox{Command: "sandbox-exec"}
}

func (s *SeatbeltSandbox) Wrap(cmd *exec.Cmd, opts SandboxOptions) error {
	command := s.Command
	if command == "" {
		command = "sandbox-exec"
	}
	if _, err := exec.LookPath(command); err != nil {
		return unsupportedSandbox{reason: err.Error()}.Wrap(cmd, opts)
	}
	profile := seatbeltProfile(opts)
	args := append([]string{"-p", profile}, cmd.Args...)
	cmd.Path = command
	cmd.Args = append([]string{command}, args...)
	return nil
}

func seatbeltProfile(opts SandboxOptions) string {
	readable := append(defaultReadablePathsDarwin(), opts.ReadablePaths...)
	if opts.WorkDir != "" {
		readable = append(readable, opts.WorkDir)
	}
	writable := append([]string{"/tmp", "/private/tmp"}, opts.WritablePaths...)
	if opts.WorkDir != "" {
		writable = append(writable, opts.WorkDir)
	}
	readable = uniquePaths(readable)
	writable = uniquePaths(writable)

	lines := []string{
		"(version 1)",
		"(deny default)",
		"(import \"system.sb\")",
		"(allow process-exec)",
		"(allow process-fork)",
		"(allow sysctl-read)",
		"(allow signal (target self))",
	}
	if opts.Network {
		lines = append(lines, "(allow network*)")
	}
	if len(readable) > 0 {
		lines = append(lines, "(allow file-read* "+seatbeltSubpaths(readable)+")")
	}
	if len(writable) > 0 {
		lines = append(lines, "(allow file-write* "+seatbeltSubpaths(writable)+")")
	}
	return strings.Join(lines, "\n")
}

func defaultReadablePathsDarwin() []string {
	return []string{"/bin", "/dev", "/System", "/usr"}
}

func seatbeltSubpaths(paths []string) string {
	parts := make([]string, 0, len(paths))
	for _, path := range paths {
		parts = append(parts, `(subpath "`+seatbeltEscapePath(filepath.Clean(path))+`")`)
	}
	return strings.Join(parts, " ")
}

func seatbeltEscapePath(path string) string {
	return strings.NewReplacer(`\`, `\\`, `"`, `\"`).Replace(path)
}
