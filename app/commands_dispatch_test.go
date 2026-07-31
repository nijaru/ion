package app

import (
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
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

func TestLogoutProviderReturnsTerminalCommand(t *testing.T) {
	previous := saveProviderKey
	t.Cleanup(func() { saveProviderKey = previous })
	var savedProvider, savedKey string
	saveProviderKey = func(provider, key string) error {
		savedProvider, savedKey = provider, key
		return nil
	}

	model := readyModel(t)
	model.Model.Config = &config.Config{Provider: "openrouter"}

	updated, cmd := model.logoutProvider()
	if cmd == nil {
		t.Fatal("logout returned no asynchronous command")
	}
	message := cmd()
	msg, ok := message.(logoutProviderSavedMsg)
	if !ok {
		t.Fatalf("logout result = %T, want logoutProviderSavedMsg", message)
	}
	if savedProvider != "openrouter" || savedKey != "" {
		t.Fatalf("saved credential = %q/%q, want openrouter/empty", savedProvider, savedKey)
	}
	_, terminalCmd := updated.Update(msg)
	requireTerminalCommitContains(t, terminalCmd, "Logged out from openrouter")
}

func TestStaleLogoutCompletionCannotReportThroughReplacement(t *testing.T) {
	previous := saveProviderKey
	t.Cleanup(func() { saveProviderKey = previous })
	saveStarted := make(chan struct{})
	releaseSave := make(chan struct{})
	saveProviderKey = func(string, string) error {
		close(saveStarted)
		<-releaseSave
		return nil
	}

	model := readyModel(t)
	model.Model.Config = &config.Config{Provider: "openrouter"}
	updated, cmd := model.logoutProvider()
	if cmd == nil {
		t.Fatal("logout returned no asynchronous command")
	}
	select {
	case <-saveStarted:
		t.Fatal("logout save started during command handling")
	default:
	}
	requestID := updated.Model.RuntimeSwitchRequest
	generation := updated.Model.EventGeneration
	messageCh := make(chan any, 1)
	go func() { messageCh <- cmd() }()
	select {
	case <-saveStarted:
	case <-time.After(time.Second):
		t.Fatal("logout command did not start save")
	}
	updated.rotateRuntimeContext()
	updated.runtimeRequest().clear()
	updated.Model.EventGeneration++
	close(releaseSave)

	message := <-messageCh
	msg, ok := message.(logoutProviderSavedMsg)
	if !ok {
		t.Fatalf("logout result = %T, want logoutProviderSavedMsg", message)
	}
	if msg.requestID != requestID || msg.generation != generation {
		t.Fatalf(
			"logout result fence = generation %d/request %d, want %d/%d",
			msg.generation,
			msg.requestID,
			generation,
			requestID,
		)
	}
	_, terminalCmd := updated.Update(msg)
	if terminalCmd != nil {
		t.Fatal("stale logout completion returned a terminal command")
	}
}
