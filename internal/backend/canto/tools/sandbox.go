package tools

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strings"
)

type SandboxMode string

const (
	SandboxOff        SandboxMode = "off"
	SandboxAuto       SandboxMode = "auto"
	SandboxSeatbelt    SandboxMode = "seatbelt"
	SandboxBubblewrap  SandboxMode = "bubblewrap"
)

var (
	sandboxGOOS     = runtime.GOOS
	sandboxLookPath  = exec.LookPath
)

type sandboxPlan struct {
	name    string
	args    []string
	dir     string
	cleanup func() error
}

func resolveSandboxMode() SandboxMode {
	raw := strings.ToLower(strings.TrimSpace(os.Getenv("ION_SANDBOX")))
	switch raw {
	case "":
		return SandboxOff
	case string(SandboxAuto):
		return SandboxAuto
	case string(SandboxOff):
		return SandboxOff
	case string(SandboxSeatbelt):
		return SandboxSeatbelt
	case string(SandboxBubblewrap):
		return SandboxBubblewrap
	default:
		return SandboxAuto
	}
}

func planSandboxedBash(cwd, command string, mode SandboxMode) (sandboxPlan, error) {
	switch mode {
	case SandboxOff:
		return sandboxPlan{
			name: "bash",
			args: []string{"-c", command},
			dir:  cwd,
		}, nil
	case SandboxSeatbelt:
		return planSeatbeltSandbox(cwd, command)
	case SandboxBubblewrap:
		return planBubblewrapSandbox(cwd, command)
	case SandboxAuto:
		if sandboxGOOS == "darwin" {
			if _, err := sandboxLookPath("sandbox-exec"); err == nil {
				return planSeatbeltSandbox(cwd, command)
			}
		}
		if sandboxGOOS == "linux" {
			if _, err := sandboxLookPath("bwrap"); err == nil {
				return planBubblewrapSandbox(cwd, command)
			}
		}
		return sandboxPlan{
			name: "bash",
			args: []string{"-c", command},
			dir:  cwd,
		}, nil
	default:
		return sandboxPlan{}, fmt.Errorf("unsupported sandbox mode %q", mode)
	}
}

func planSeatbeltSandbox(cwd, command string) (sandboxPlan, error) {
	profile, err := os.CreateTemp("", "ion-seatbelt-*.sb")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("create seatbelt profile: %w", err)
	}
	profileText := seatbeltProfile(cwd)
	if _, err := profile.WriteString(profileText); err != nil {
		_ = profile.Close()
		_ = os.Remove(profile.Name())
		return sandboxPlan{}, fmt.Errorf("write seatbelt profile: %w", err)
	}
	if err := profile.Close(); err != nil {
		_ = os.Remove(profile.Name())
		return sandboxPlan{}, fmt.Errorf("close seatbelt profile: %w", err)
	}
	return sandboxPlan{
		name: "sandbox-exec",
		args: []string{"-f", profile.Name(), "/bin/bash", "-c", command},
		dir:  cwd,
		cleanup: func() error {
			return os.Remove(profile.Name())
		},
	}, nil
}

func seatbeltProfile(cwd string) string {
	escaped := strings.ReplaceAll(cwd, "\"", "\\\"")
	return fmt.Sprintf(`(version 1)
(deny default)
(allow process*)
(allow signal (target self))
(allow file-read*
  (subpath "/bin")
  (subpath "/usr/bin")
  (subpath "/usr/lib")
  (subpath "/System")
  (subpath "/Library")
  (subpath "/etc")
  (subpath "/private/etc")
  (subpath "/tmp")
  (subpath "/private/tmp")
  (subpath "%s"))
(allow file-write*
  (subpath "/tmp")
  (subpath "/private/tmp")
  (subpath "%s"))
`, escaped, escaped)
}

func planBubblewrapSandbox(cwd, command string) (sandboxPlan, error) {
	bwrap, err := sandboxLookPath("bwrap")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("bubblewrap unavailable: %w", err)
	}
	args := []string{
		"--unshare-net",
		"--bind", cwd, cwd,
		"--chdir", cwd,
		"--ro-bind", "/bin", "/bin",
		"--ro-bind", "/usr", "/usr",
		"--ro-bind", "/etc", "/etc",
		"--bind", "/tmp", "/tmp",
		"--bind", "/private/tmp", "/private/tmp",
		"--dev", "/dev",
		"--proc", "/proc",
		"/bin/bash", "-c", command,
	}
	if sandboxGOOS == "darwin" {
		args = append(args,
			"--ro-bind", "/System", "/System",
			"--ro-bind", "/Library", "/Library",
		)
	}
	return sandboxPlan{
		name: bwrap,
		args: args,
		dir:  cwd,
	}, nil
}
