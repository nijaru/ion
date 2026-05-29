//go:build darwin

package safety

import (
	"os/exec"
	"strings"
	"testing"
)

func TestSeatbeltSandboxWrapPrefixesSandboxExec(t *testing.T) {
	cmd := exec.Command("bash", "-lc", "echo hi")
	sandbox := &SeatbeltSandbox{Command: "sandbox-exec"}

	err := sandbox.Wrap(cmd, SandboxOptions{
		WorkDir:       "/tmp/work",
		ReadablePaths: []string{"/tmp/work"},
		WritablePaths: []string{"/tmp/work"},
	})
	if err != nil {
		if !strings.Contains(err.Error(), "sandbox backend unavailable") &&
			!strings.Contains(err.Error(), "executable file not found") {
			t.Fatalf("Wrap: %v", err)
		}
		return
	}
	if cmd.Path != "sandbox-exec" {
		t.Fatalf("Path = %q, want sandbox-exec", cmd.Path)
	}
	if len(cmd.Args) < 4 || cmd.Args[0] != "sandbox-exec" || cmd.Args[1] != "-p" {
		t.Fatalf("unexpected wrapped args: %#v", cmd.Args)
	}
	if !strings.Contains(cmd.Args[2], "(allow file-write*") {
		t.Fatalf("expected write allowance in profile, got %q", cmd.Args[2])
	}
}

func TestSeatbeltProfileEscapesPathStrings(t *testing.T) {
	profile := seatbeltProfile(SandboxOptions{
		ReadablePaths: []string{`/tmp/work"quote`, `/tmp/work\slash`},
	})
	if !strings.Contains(profile, `(subpath "/tmp/work\"quote")`) {
		t.Fatalf("expected quote to be escaped in profile, got %q", profile)
	}
	if !strings.Contains(profile, `(subpath "/tmp/work\\slash")`) {
		t.Fatalf("expected backslash to be escaped in profile, got %q", profile)
	}
}
