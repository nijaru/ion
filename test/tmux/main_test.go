package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type tmuxTest struct {
	t       *testing.T
	socket  string
	session string
	root    string
	binary  string
}

func newTmuxTest(t *testing.T) *tmuxTest {
	t.Helper()
	session := fmt.Sprintf("ion-test-%d", time.Now().UnixNano())
	socket := fmt.Sprintf("/tmp/tmux-ion-%d.sock", time.Now().UnixNano())
	cwd, _ := os.Getwd()
	moduleRoot := cwd
	for {
		if _, err := os.Stat(moduleRoot + "/go.mod"); err == nil {
			break
		}
		parent := moduleRoot[:strings.LastIndex(moduleRoot, "/")]
		if parent == "" || parent == moduleRoot {
			break
		}
		moduleRoot = parent
	}

	cmd := exec.Command("tmux", "-S", socket, "new-session", "-d", "-s", session,
		"-x", "120", "-y", "40",
		"--", "/bin/bash", "-c", "cd "+moduleRoot+" && exec bash")
	env := os.Environ()
	var filtered []string
	for _, e := range env {
		if !strings.HasPrefix(e, "TMUX=") {
			filtered = append(filtered, e)
		}
	}
	cmd.Env = filtered
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("tmux new-session: %v\n%s", err, out)
	}
	time.Sleep(500 * time.Millisecond)

	if out, err := exec.Command("tmux", "-S", socket, "has-session", "-t", session).CombinedOutput(); err != nil {
		t.Fatalf("session not found: %v\n%s", err, out)
	}

	binaryPath := filepath.Join(t.TempDir(), "ion")
	build := exec.Command("go", "build", "-o", binaryPath, "./cmd/ion")
	build.Dir = moduleRoot
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build Ion for tmux acceptance: %v\n%s", err, out)
	}

	tt := &tmuxTest{
		t:       t,
		socket:  socket,
		session: session,
		root:    moduleRoot,
		binary:  binaryPath,
	}
	t.Cleanup(func() {
		tt.tmuxSafe("kill-session", "-t", session)
		tt.tmuxSafe("kill-server")
		os.Remove(socket)
	})
	return tt
}

func (tt *tmuxTest) tmux(args ...string) {
	tt.t.Helper()
	fullArgs := append([]string{"-S", tt.socket}, args...)
	cmd := exec.Command("tmux", fullArgs...)
	cmd.Env = os.Environ()
	out, err := cmd.CombinedOutput()
	if err != nil {
		tt.t.Fatalf("tmux %v: %v\n%s", args, err, out)
	}
	_ = out
}

func (tt *tmuxTest) tmuxSafe(args ...string) {
	fullArgs := append([]string{"-S", tt.socket}, args...)
	cmd := exec.Command("tmux", fullArgs...)
	cmd.Env = os.Environ()
	cmd.Run() // ignore errors
}

func (tt *tmuxTest) capture() string {
	tt.t.Helper()
	cmd := exec.Command("tmux", "-S", tt.socket, "capture-pane", "-t", tt.session, "-p")
	out, err := cmd.Output()
	if err != nil {
		return ""
	}
	return string(out)
}

func (tt *tmuxTest) sendKeys(keys ...string) {
	tt.t.Helper()
	args := append([]string{"send-keys", "-t", tt.session}, keys...)
	tt.tmux(args...)
}

func (tt *tmuxTest) typeText(text string) {
	tt.t.Helper()
	escaped := strings.NewReplacer(";", "\\;", "\"", "\\\"").Replace(text)
	tt.tmux("send-keys", "-t", tt.session, "-l", escaped)
}

func (tt *tmuxTest) enter() {
	tt.t.Helper()
	tt.tmux("send-keys", "-t", tt.session, "Enter")
}

func (tt *tmuxTest) send(text string) {
	tt.t.Helper()
	tt.typeText(text)
	tt.enter()
}

func (tt *tmuxTest) waitFor(substr string, timeout time.Duration) string {
	tt.t.Helper()
	deadline := time.After(timeout)
	for {
		select {
		case <-deadline:
			content := tt.capture()
			tt.t.Fatalf("timeout waiting for %q after %v\nContent:\n%s", substr, timeout, content)
			return content
		default:
			content := tt.capture()
			if strings.Contains(content, substr) {
				return content
			}
			time.Sleep(500 * time.Millisecond)
		}
	}
}

// launchIon starts Ion with --no-session and waits for the composer prompt.
func (tt *tmuxTest) launchIon() {
	tt.t.Helper()
	tt.typeText(tt.binary + " --no-session")
	tt.enter()
	tt.waitFor("Type a message", 15*time.Second)
}

// TestTUIInteractive verifies Ion starts and produces an assistant response.
func TestTUIInteractive(t *testing.T) {
	if os.Getenv("ION_TMUX_LIVE") != "1" {
		t.Skip("live tmux acceptance requires ION_TMUX_LIVE=1")
	}
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not available")
	}

	tt := newTmuxTest(t)
	tt.launchIon()
	t.Log("Ion started")

	tt.send("Which planet is third from the Sun? Reply with exactly one word and no punctuation.")
	t.Log("sent prompt, waiting for committed assistant response...")

	// Wait for the committed assistant response, not the user prompt. The
	// expected word must not appear in the prompt so this cannot pass early.
	content := tt.waitFor("• Earth", 60*time.Second)
	t.Logf("full output after turn:\n%s", content)

	if !strings.Contains(content, "• Earth") {
		t.Fatalf("expected committed Earth response in terminal output")
	}

	tt.sendKeys("C-c")
	time.Sleep(1 * time.Second)
}

// TestBuiltBinaryInteractiveLocalProvider proves the compiled executable can
// load configuration, start the TUI, stream a provider response, and shut down
// through a real tmux PTY without requiring external credentials.
func TestBuiltBinaryInteractiveLocalProvider(t *testing.T) {
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not available")
	}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodGet && r.URL.Path == "/v1/models" {
			w.Header().Set("Content-Type", "application/json")
			_, _ = fmt.Fprint(w, `{"data":[]}`)
			return
		}
		if r.Method != http.MethodPost || r.URL.Path != "/v1/chat/completions" {
			http.Error(w, "unexpected request", http.StatusNotFound)
			return
		}
		var request struct {
			Model string `json:"model"`
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			http.Error(w, fmt.Sprintf("decode request: %v", err), http.StatusBadRequest)
			return
		}
		if request.Model != "fake-model" || r.Header.Get("Authorization") != "Bearer fake-key" {
			http.Error(w, "unexpected provider request", http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		w.Header().Set("Cache-Control", "no-cache")
		_, _ = fmt.Fprint(
			w,
			"data: {\"id\":\"built-tui-test\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"built-\"}}]}\n\n",
			"data: {\"id\":\"built-tui-test\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"tui-ok\"},\"finish_reason\":\"stop\"}]}\n\n",
			"data: [DONE]\n\n",
		)
	}))
	defer server.Close()

	home := t.TempDir()
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o700); err != nil {
		t.Fatal(err)
	}
	config := fmt.Sprintf(
		"provider = 'openai-compatible'\nmodel = 'fake-model'\nendpoint = '%s/v1'\nauth_env_var = 'ION_TMUX_TEST_KEY'\n",
		server.URL,
	)
	if err := os.WriteFile(filepath.Join(configDir, "config.toml"), []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HOME", home)
	t.Setenv("ION_PROVIDER", "")
	t.Setenv("ION_MODEL", "")
	t.Setenv("ION_REASONING_EFFORT", "")
	t.Setenv("ION_TMUX_TEST_KEY", "fake-key")

	tt := newTmuxTest(t)
	tt.typeText(tt.binary + " --trust --no-session")
	tt.enter()
	tt.waitFor("Type a message", 15*time.Second)
	tt.send("return the built TUI marker")
	content := tt.waitFor("built-tui-ok", 30*time.Second)
	if !strings.Contains(content, "built-tui-ok") {
		t.Fatalf("built Ion TUI output = %q, want streamed marker", content)
	}
	tt.sendKeys("C-c")
	time.Sleep(1 * time.Second)
}

// TestTUIToolCall verifies Ion can execute a Read tool and display the result.
func TestTUIToolCall(t *testing.T) {
	if os.Getenv("ION_TMUX_LIVE") != "1" {
		t.Skip("live tmux acceptance requires ION_TMUX_LIVE=1")
	}
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not available")
	}

	tt := newTmuxTest(t)
	testFile := filepath.Join(tt.root, fmt.Sprintf(".ion-tmux-test-readme-%d.txt", time.Now().UnixNano()))
	if err := os.WriteFile(testFile, []byte("ION-TMUX-FILE-CONTENT-42"), 0o644); err != nil {
		t.Fatal(err)
	}
	defer os.Remove(testFile)

	tt.launchIon()
	t.Log("Ion started")

	tt.send(
		"Read the file " + testFile + " and tell me exactly what it contains. Say only the file content, nothing else.",
	)
	t.Log("sent read request, waiting for tool execution...")

	content := tt.waitFor("ION-TMUX-FILE-CONTENT-42", 60*time.Second)
	t.Logf("got response:\n%s", content)

	if !strings.Contains(content, "ION-TMUX-FILE-CONTENT-42") {
		t.Fatalf("expected ION-TMUX-FILE-CONTENT-42 in response")
	}

	tt.sendKeys("C-c")
	time.Sleep(1 * time.Second)
}
