package tool

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
)

type SandboxMode string

const (
	SandboxOff        SandboxMode = "off"
	SandboxAuto       SandboxMode = "auto"
	SandboxSeatbelt   SandboxMode = "seatbelt"
	SandboxBubblewrap SandboxMode = "bubblewrap"
)

var (
	sandboxGOOS       = runtime.GOOS
	sandboxLookPath   = exec.LookPath
	sandboxPathExists = func(path string) bool {
		_, err := os.Stat(path)
		return err == nil
	}
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
		return SandboxAuto
	case string(SandboxAuto):
		return SandboxAuto
	case string(SandboxOff):
		return SandboxOff
	case string(SandboxSeatbelt):
		return SandboxSeatbelt
	case string(SandboxBubblewrap):
		return SandboxBubblewrap
	default:
		return SandboxMode("invalid")
	}
}

func SandboxSummary() string {
	return sandboxSummary(resolveSandboxMode())
}

func sandboxNetworkIntent(mode SandboxMode) string {
	if mode == SandboxOff {
		return "unrestricted"
	}
	return "denied-by-sandbox"
}

func sandboxSummary(mode SandboxMode) string {
	switch mode {
	case SandboxOff:
		return string(SandboxOff)
	case SandboxSeatbelt:
		if sandboxGOOS != "darwin" {
			return "seatbelt unavailable on " + sandboxGOOS
		}
		if _, err := sandboxLookPath("sandbox-exec"); err != nil {
			return "seatbelt unavailable"
		}
		return string(SandboxSeatbelt)
	case SandboxBubblewrap:
		if sandboxGOOS != "linux" {
			return "bubblewrap unavailable on " + sandboxGOOS
		}
		if _, err := sandboxLookPath("bwrap"); err != nil {
			return "bubblewrap unavailable"
		}
		return string(SandboxBubblewrap)
	case SandboxAuto:
		if sandboxGOOS == "darwin" {
			if _, err := sandboxLookPath("sandbox-exec"); err == nil {
				return "auto: seatbelt"
			}
		}
		if sandboxGOOS == "linux" {
			if _, err := sandboxLookPath("bwrap"); err == nil {
				return "auto: bubblewrap"
			}
		}
		return "auto: unavailable"
	default:
		return "unsupported: " + string(mode)
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
		return sandboxPlan{}, fmt.Errorf("automatic sandbox backend unavailable on %s", sandboxGOOS)
	default:
		return sandboxPlan{}, fmt.Errorf("unsupported sandbox mode %q", mode)
	}
}

func planSeatbeltSandbox(cwd, command string) (sandboxPlan, error) {
	if sandboxGOOS != "darwin" {
		return sandboxPlan{}, fmt.Errorf("seatbelt sandbox unsupported on %s", sandboxGOOS)
	}
	seatbelt, err := sandboxLookPath("sandbox-exec")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("seatbelt sandbox unavailable: %w", err)
	}
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
		name: seatbelt,
		args: []string{"-f", profile.Name(), "/bin/bash", "-c", command},
		dir:  cwd,
		cleanup: func() error {
			return os.Remove(profile.Name())
		},
	}, nil
}

func seatbeltProfile(cwd string) string {
	quoted := strconv.Quote(cwd)
	// macOS system processes may resolve dependencies outside a finite list of
	// stable paths (for example, through dyld caches and private framework paths).
	// A filtered read rule aborts sandbox-exec on those resolutions, so keep reads
	// unrestricted while retaining the write and network restrictions below.
	return fmt.Sprintf(`(version 1)
(deny default)
(allow process*)
(allow signal (target self))
(allow file-read*)
(allow file-read* (subpath %s))
(allow file-write*
  (subpath "/tmp")
  (subpath "/private/tmp")
  (subpath %s))
`, quoted, quoted)
}

func planBubblewrapSandbox(cwd, command string) (sandboxPlan, error) {
	if sandboxGOOS != "linux" {
		return sandboxPlan{}, fmt.Errorf("bubblewrap sandbox unsupported on %s", sandboxGOOS)
	}
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
		"--dev", "/dev",
		"--proc", "/proc",
	}
	if sandboxPathExists("/private/tmp") {
		args = append(args, "--bind", "/private/tmp", "/private/tmp")
	}
	if sandboxGOOS == "darwin" {
		args = append(
			args,
			"--ro-bind", "/System", "/System",
			"--ro-bind", "/Library", "/Library",
		)
	}
	args = append(args, "/bin/bash", "-c", command)
	return sandboxPlan{
		name: bwrap,
		args: args,
		dir:  cwd,
	}, nil
}
