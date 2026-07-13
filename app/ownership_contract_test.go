package app

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
)

func TestRuntimeSwitchInstallsHarnessRunner(t *testing.T) {
	model := readyModel(t)
	model.Model.RuntimeSwitchRequest = 1
	oldRunner := &stubRunner{}
	newRunner := &stubRunner{}
	model.Model.Runner = oldRunner

	model, _ = model.handleRuntimeSwitched(runtimeSwitchedMsg{
		switchID: 1,
		runtime:  Accepted{Handles: Handles{Runner: newRunner}},
		previous: Handles{Runner: oldRunner},
	})
	if model.Model.Runner != newRunner {
		t.Fatalf("runner = %p, want switched harness %p", model.Model.Runner, newRunner)
	}
}

func TestTUIOwnershipGuards(t *testing.T) {
	appDir, err := os.Getwd()
	if err != nil {
		t.Fatalf("locate app package: %v", err)
	}
	entries, err := os.ReadDir(appDir)
	if err != nil {
		t.Fatalf("read app package: %v", err)
	}
	for _, entry := range entries {
		name := entry.Name()
		if entry.IsDir() || !strings.HasSuffix(name, ".go") || strings.HasSuffix(name, "_test.go") {
			continue
		}
		path := filepath.Join(appDir, name)
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("read %s: %v", name, err)
		}
		source := string(data)
		if strings.Contains(source, strings.Join([]string{"internal", "runtime"}, "/")) {
			t.Fatalf("%s imports deleted runtime package ownership", name)
		}
		if strings.Contains(source, "agent.New") {
			t.Fatalf("%s constructs an agent; the harness is the sole runtime owner", name)
		}
		if regexp.MustCompile(`(?m)\bgo\s+func`).MatchString(source) {
			t.Fatalf("%s starts a goroutine; the TUI must remain projection/control only", name)
		}
		if strings.Contains(source, "[]session.Message") {
			t.Fatalf("%s stores a second message transcript", name)
		}
	}
}
