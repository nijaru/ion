package tool

import (
	"context"
	"errors"
	"os"
	"os/exec"
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
