package main

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"
)

type tmuxTest struct {
	t       *testing.T
	socket  string
	session string
}

func newTmuxTest(t *testing.T) *tmuxTest {
	t.Helper()
	session := fmt.Sprintf("ion-test-%d", time.Now().UnixNano())
	socket := fmt.Sprintf("/tmp/tmux-ion-%d.sock", time.Now().UnixNano())
	cwd, _ := os.Getwd()
	// Walk up to module root (go test sets cwd to package dir).
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

	// Start a detached tmux session with a shell that stays alive.
	cmd := exec.Command("tmux", "-S", socket, "new-session", "-d", "-s", session,
		"-x", "120", "-y", "40",
		"--", "/bin/bash", "-c", "cd "+moduleRoot+" && exec bash")
	// Unset TMUX to avoid nested tmux issues.
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

	// Verify session exists.
	if out, err := exec.Command("tmux", "-S", socket, "has-session", "-t", session).CombinedOutput(); err != nil {
		t.Fatalf("session not found: %v\n%s", err, out)
	}

	tt := &tmuxTest{t: t, socket: socket, session: session}

	t.Cleanup(func() {
		tt.tmuxSafe("kill-session", "-t", session)
		tt.tmuxSafe("kill-server")
		os.Remove(socket)
	})

	return tt
}

func (tt *tmuxTest) tmux(args ...string) string {
	tt.t.Helper()
	fullArgs := append([]string{"-S", tt.socket}, args...)
	cmd := exec.Command("tmux", fullArgs...)
	cmd.Env = os.Environ()
	out, err := cmd.CombinedOutput()
	if err != nil {
		tt.t.Fatalf("tmux %v: %v\n%s", args, err, out)
	}
	return string(out)
}

func (tt *tmuxTest) typeText(text string) {
	tt.t.Helper()
	// Escape special characters for tmux send-keys.
	escaped := strings.NewReplacer(";", "\\;", "\"", "\\\"").Replace(text)
	tt.tmux("send-keys", "-t", tt.session, "-l", escaped)
}

func (tt *tmuxTest) pressEnter() {
	tt.t.Helper()
	tt.tmux("send-keys", "-t", tt.session, "Enter")
}

func (tt *tmuxTest) send(text string) {
	tt.t.Helper()
	tt.typeText(text)
	tt.pressEnter()
}

func (tt *tmuxTest) sendKeys(keys ...string) {
	tt.t.Helper()
	args := append([]string{"send-keys", "-t", tt.session}, keys...)
	tt.tmux(args...)
}

func (tt *tmuxTest) captureRaw() (string, error) {
	tt.t.Helper()
	cmd := exec.Command("tmux", "-S", tt.socket, "capture-pane", "-t", tt.session, "-p")
	out, err := cmd.Output()
	return string(out), err
}

func (tt *tmuxTest) capture() string {
	tt.t.Helper()
	out, err := tt.captureRaw()
	if err != nil {
		return ""
	}
	return out
}

func (tt *tmuxTest) tmuxSafe(args ...string) string {
	fullArgs := append([]string{"-S", tt.socket}, args...)
	cmd := exec.Command("tmux", fullArgs...)
	cmd.Env = os.Environ()
	out, _ := cmd.CombinedOutput()
	return string(out)
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

// TestTUIInteractive verifies Ion starts from bash, accepts input, and produces output.
func TestTUIInteractive(t *testing.T) {
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not available")
	}

	tt := newTmuxTest(t)

	// Launch Ion inside the tmux session. The shell's cwd is the module root.
	tt.typeText("./ion --no-session")
	tt.pressEnter()

	// Wait for Ion to initialize.
	content := tt.waitFor("Type a message", 15*time.Second)
	t.Logf("startup:\n%s", content)

	// Send a prompt.
	tt.send("Say exactly: HELLO-TMUX-TEST")
	t.Log("sent prompt, waiting for response...")

	content = tt.waitFor("HELLO-TMUX-TEST", 60*time.Second)
	t.Logf("response:\n%s", content)

	if !strings.Contains(content, "HELLO-TMUX-TEST") {
		t.Fatalf("response does not contain expected text\nContent:\n%s", content)
	}

	// Quit.
	tt.sendKeys("C-c")
	time.Sleep(1 * time.Second)
}
