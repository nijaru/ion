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
	root    string
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

	tt := &tmuxTest{t: t, socket: socket, session: session, root: moduleRoot}
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

// waitForGone waits until substr disappears from the captured pane.
func (tt *tmuxTest) waitForGone(substr string, timeout time.Duration) {
	tt.t.Helper()
	deadline := time.After(timeout)
	for {
		select {
		case <-deadline:
			tt.t.Fatalf("timeout waiting for %q to disappear after %v", substr, timeout)
		default:
			content := tt.capture()
			if !strings.Contains(content, substr) {
				return
			}
			time.Sleep(500 * time.Millisecond)
		}
	}
}

// launchIon starts Ion with --no-session and waits for the composer prompt.
func (tt *tmuxTest) launchIon() {
	tt.t.Helper()
	tt.typeText("./ion --no-session")
	tt.enter()
	tt.waitFor("Type a message", 15*time.Second)
}

// TestTUIInteractive verifies Ion starts and produces an assistant response.
func TestTUIInteractive(t *testing.T) {
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not available")
	}

	tt := newTmuxTest(t)
	tt.launchIon()
	t.Log("Ion started")

	tt.send("Say the word BANANA exactly, nothing else.")
	t.Log("sent prompt, waiting for response...")

	// Wait for the durable assistant response. The transient Submitting status
	// may be shorter than the capture polling interval with a fast provider.
	content := tt.waitFor("BANANA", 60*time.Second)
	t.Logf("full output after turn:\n%s", content)

	if !strings.Contains(content, "BANANA") {
		t.Fatalf("expected BANANA in terminal output")
	}

	tt.sendKeys("C-c")
	time.Sleep(1 * time.Second)
}

// TestTUIToolCall verifies Ion can execute a Read tool and display the result.
func TestTUIToolCall(t *testing.T) {
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not available")
	}

	testFile := "/tmp/ion-tmux-test-readme.txt"
	if err := os.WriteFile(testFile, []byte("ION-TMUX-FILE-CONTENT-42"), 0644); err != nil {
		t.Fatal(err)
	}
	defer os.Remove(testFile)

	tt := newTmuxTest(t)
	tt.launchIon()
	t.Log("Ion started")

	tt.send("Read the file " + testFile + " and tell me exactly what it contains. Say only the file content, nothing else.")
	t.Log("sent read request, waiting for tool execution...")

	content := tt.waitFor("ION-TMUX-FILE-CONTENT-42", 60*time.Second)
	t.Logf("got response:\n%s", content)

	if !strings.Contains(content, "ION-TMUX-FILE-CONTENT-42") {
		t.Fatalf("expected ION-TMUX-FILE-CONTENT-42 in response")
	}

	tt.sendKeys("C-c")
	time.Sleep(1 * time.Second)
}
