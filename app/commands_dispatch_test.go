package app

import (
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
)

func TestSlashCommandDispatchIsExhaustive(t *testing.T) {
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime caller unavailable")
	}
	dir := filepath.Dir(file)
	if !filepath.IsAbs(dir) {
		dir = "."
	}
	source, err := os.ReadFile(filepath.Join(dir, "commands_dispatch.go"))
	if err != nil {
		t.Fatalf("read dispatch: %v", err)
	}
	var cases []string
	for _, line := range strings.Split(string(source), "\n") {
		if strings.HasPrefix(strings.TrimSpace(line), "case ") {
			cases = append(cases, line)
		}
	}
	for _, command := range SlashCommandCatalog() {
		quoted := strconv.Quote(command.Name)
		found := false
		for _, line := range cases {
			if strings.Contains(line, quoted) {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("slash command %q has no dispatch case", command.Name)
		}
	}
}
