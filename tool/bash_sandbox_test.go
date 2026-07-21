package tool

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestSeatbeltBashRunsWhenBackendAvailable(t *testing.T) {
	if runtime.GOOS != "darwin" {
		t.Skip("seatbelt is only available on macOS")
	}
	if _, err := exec.LookPath("sandbox-exec"); err != nil {
		t.Skipf("sandbox-exec unavailable: %v", err)
	}

	previousMode, hadMode := os.LookupEnv("ION_SANDBOX")
	t.Cleanup(func() {
		if hadMode {
			_ = os.Setenv("ION_SANDBOX", previousMode)
		} else {
			_ = os.Unsetenv("ION_SANDBOX")
		}
	})
	if err := os.Setenv("ION_SANDBOX", string(SandboxSeatbelt)); err != nil {
		t.Fatal(err)
	}

	output, err := NewBash(t.TempDir()).Execute(t.Context(), `{"command":"printf sandbox-ok"}`)
	if err != nil {
		t.Fatalf("Execute error = %v", err)
	}
	if output != "sandbox-ok" {
		t.Fatalf("output = %q, want sandbox-ok", output)
	}
}

func TestSeatbeltBashCannotReadOutsideWorkspace(t *testing.T) {
	if runtime.GOOS != "darwin" {
		t.Skip("seatbelt is only available on macOS")
	}
	if _, err := exec.LookPath("sandbox-exec"); err != nil {
		t.Skipf("sandbox-exec unavailable: %v", err)
	}
	t.Setenv("ION_SANDBOX", string(SandboxSeatbelt))

	workspace := t.TempDir()
	outside := t.TempDir()
	secretPath := filepath.Join(outside, "credential.txt")
	if err := os.WriteFile(secretPath, []byte("outside-secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	command := "cat '" + strings.ReplaceAll(secretPath, "'", "'\\''") + "'"
	args, err := json.Marshal(map[string]string{"command": command})
	if err != nil {
		t.Fatal(err)
	}
	output, runErr := NewBash(workspace).Execute(t.Context(), string(args))
	if runErr == nil {
		t.Fatalf("outside read unexpectedly succeeded with output %q", output)
	}
	if strings.Contains(output, "outside-secret") {
		t.Fatalf("outside secret leaked through sandbox output: %q", output)
	}
}

func TestNewBashAppliesExplicitSandboxMode(t *testing.T) {
	previousMode, hadMode := os.LookupEnv("ION_SANDBOX")
	t.Cleanup(func() {
		if hadMode {
			_ = os.Setenv("ION_SANDBOX", previousMode)
		} else {
			_ = os.Unsetenv("ION_SANDBOX")
		}
	})
	if err := os.Setenv("ION_SANDBOX", string(SandboxSeatbelt)); err != nil {
		t.Fatal(err)
	}

	previousGOOS := sandboxGOOS
	previousLookPath := sandboxLookPath
	t.Cleanup(func() {
		sandboxGOOS = previousGOOS
		sandboxLookPath = previousLookPath
	})
	sandboxGOOS = "darwin"
	sandboxLookPath = func(string) (string, error) {
		return "", errors.New("sandbox unavailable")
	}

	_, err := NewBash(t.TempDir()).Execute(context.Background(), `{"command":"echo should not run"}`)
	if err == nil || !strings.Contains(err.Error(), "seatbelt sandbox unavailable") {
		t.Fatalf("Execute error = %v, want explicit seatbelt failure", err)
	}
}
