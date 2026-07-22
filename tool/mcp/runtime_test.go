package mcp

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"syscall"
	"testing"
	"time"

	sdkmcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

func TestMCPHelperProcess(t *testing.T) {
	if os.Getenv("ION_MCP_HELPER") != "1" {
		return
	}
	srv, _ := newTestServer()
	if err := srv.Serve(context.Background(), os.Stdin, os.Stdout); err != nil && !errors.Is(err, context.Canceled) {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	os.Exit(0)
}

func TestOwnedClientCloseIgnoresExpectedShutdownErrors(t *testing.T) {
	for _, err := range []error{context.Canceled, sdkmcp.ErrConnectionClosed} {
		t.Run(err.Error(), func(t *testing.T) {
			client := &ownedClient{
				client: &Client{session: &fakeClientSession{closeErr: err}},
				cancel: func() {},
			}
			if err := client.Close(); err != nil {
				t.Fatalf("ownedClient.Close() = %v, want nil for expected shutdown", err)
			}
		})
	}
}

func TestOwnedClientCloseTerminatesProcessGroup(t *testing.T) {
	command := exec.Command("/bin/sh", "-c", "sleep 30")
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := command.Start(); err != nil {
		t.Fatalf("start child: %v", err)
	}
	cleaned := false
	client := &ownedClient{
		client:  &Client{session: &fakeClientSession{closeErr: context.Canceled}},
		command: command,
		cleanup: func() error {
			cleaned = true
			return nil
		},
	}
	if err := client.Close(); err != nil {
		t.Fatalf("ownedClient.Close() = %v, want nil", err)
	}
	if err := command.Wait(); err == nil {
		t.Fatal("process group leader exited without termination")
	}
	if !cleaned {
		t.Fatal("ownedClient.Close did not run sandbox cleanup")
	}
}

func TestOwnedClientCloseSurfacesSandboxCleanupFailure(t *testing.T) {
	command := exec.Command("/bin/sh", "-c", "sleep 30")
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := command.Start(); err != nil {
		t.Fatalf("start child: %v", err)
	}
	cleanupErr := errors.New("sandbox cleanup failed")
	cleaned := false
	client := &ownedClient{
		client:  &Client{session: &fakeClientSession{}},
		command: command,
		cleanup: func() error {
			cleaned = true
			return cleanupErr
		},
	}

	err := client.Close()
	if !errors.Is(err, cleanupErr) {
		t.Fatalf("ownedClient.Close() = %v, want sandbox cleanup error", err)
	}
	if !cleaned {
		t.Fatal("ownedClient.Close did not run sandbox cleanup after process termination")
	}
	if err := command.Wait(); err == nil {
		t.Fatal("process group leader exited cleanly; expected termination")
	}
}

func TestRuntimeCloseJoinsClientCleanupFailures(t *testing.T) {
	first := errors.New("first sandbox cleanup failed")
	second := errors.New("second sandbox cleanup failed")
	runtime := &Runtime{clients: []*ownedClient{
		{
			client: &Client{session: &fakeClientSession{}},
			cleanup: func() error {
				return first
			},
		},
		{
			client: &Client{session: &fakeClientSession{}},
			cleanup: func() error {
				return second
			},
		},
	}}

	err := runtime.Close()
	if !errors.Is(err, first) || !errors.Is(err, second) {
		t.Fatalf("Runtime.Close() = %v, want both cleanup errors", err)
	}
	if err := runtime.Close(); err != nil {
		t.Fatalf("second Runtime.Close() = %v, want nil", err)
	}
}

func TestMCPParentWatchKillsGroupWhenIonPipeCloses(t *testing.T) {
	childControl, parentControl, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	commandArgs := append([]string{"-c", mcpParentWatchScript, "ion-mcp-supervisor", "/bin/sh"}, "-c", "sleep 30")
	command := exec.Command("/bin/sh", commandArgs...)
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	command.ExtraFiles = []*os.File{childControl}
	devNull, err := os.OpenFile(os.DevNull, os.O_RDWR, 0)
	if err != nil {
		childControl.Close()
		parentControl.Close()
		t.Fatal(err)
	}
	defer devNull.Close()
	command.Stdin = devNull
	command.Stdout = devNull
	command.Stderr = devNull
	if err := command.Start(); err != nil {
		childControl.Close()
		parentControl.Close()
		t.Fatal(err)
	}
	_ = childControl.Close()
	waitDone := make(chan error, 1)
	go func() { waitDone <- command.Wait() }()
	if err := parentControl.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-waitDone:
		if err == nil {
			t.Fatal("parent-watch supervisor exited cleanly; expected group termination")
		}
	case <-time.After(2 * time.Second):
		_ = syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
		<-waitDone
		t.Fatal("parent-watch supervisor did not terminate its group")
	}
}

func TestOpenDiscoversAndClosesStdioRuntime(t *testing.T) {
	runtime, err := Open(t.Context(), t.TempDir(), []ServerConfig{{
		Name:    "test",
		Command: os.Args[0],
		Args:    []string{"-test.run=TestMCPHelperProcess"},
		Env:     map[string]string{"ION_MCP_HELPER": "1"},
	}})
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	tools := runtime.Tools()
	if len(tools) != 1 || tools[0].Spec().Name != "mcp_test_echo" {
		t.Fatalf("discovered tools = %#v, want namespaced echo", tools)
	}
	if requirement, ok, err := tools[0].(interface {
		ApprovalRequirement(string) (Requirement, bool, error)
	}).ApprovalRequirement(`{"msg":"hello"}`); err != nil || !ok || !strings.HasPrefix(requirement.MCPIdentity, "mcp:server:test:") {
		t.Fatalf("external tool approval = %#v, %v, %v; want required server identity", requirement, err, ok)
	}
	text, err := tools[0].Execute(t.Context(), `{"msg":"hello"}`)
	if err != nil || text != "echo: hello" {
		t.Fatalf("external Execute = %q, %v", text, err)
	}
	canceled, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := tools[0].Execute(canceled, `{"msg":"hello"}`); err == nil {
		t.Fatal("canceled external Execute unexpectedly succeeded")
	}
	if err := runtime.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := runtime.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	replacement, err := Open(t.Context(), t.TempDir(), []ServerConfig{{
		Name:    "test",
		Command: os.Args[0],
		Args:    []string{"-test.run=TestMCPHelperProcess"},
		Env:     map[string]string{"ION_MCP_HELPER": "1"},
	}})
	if err != nil {
		t.Fatalf("rebuild Open: %v", err)
	}
	if err := replacement.Close(); err != nil {
		t.Fatalf("rebuild Close: %v", err)
	}
}

func TestOpenRejectsInvalidAndDuplicateServerNames(t *testing.T) {
	for _, configs := range [][]ServerConfig{
		{{Name: "bad name", Command: "echo"}},
		{{Name: "same", Command: "echo"}, {Name: "same", Command: "echo"}},
		{{Name: "env", Command: "echo", Env: map[string]string{"BAD=KEY": "value"}}},
	} {
		if _, err := Open(t.Context(), t.TempDir(), configs); err == nil {
			t.Fatalf("Open(%#v) unexpectedly succeeded", configs)
		}
	}
}

func TestOpenFailsClosedWhenSandboxConfigurationIsInvalid(t *testing.T) {
	t.Setenv("ION_SANDBOX", "invalid")
	if _, err := Open(t.Context(), t.TempDir(), []ServerConfig{{
		Name:    "invalid-sandbox",
		Command: "echo",
	}}); err == nil || !strings.Contains(err.Error(), "sandbox") {
		t.Fatalf("Open with invalid sandbox mode error = %v, want fail-closed sandbox error", err)
	}
}

func TestOpenFailsClosedWhenPerServerPolicyCannotBeEnforced(t *testing.T) {
	t.Setenv("ION_SANDBOX", "off")
	if _, err := Open(t.Context(), t.TempDir(), []ServerConfig{{
		Name:    "unsandboxed",
		Command: os.Args[0],
		Args:    []string{"-test.run=TestMCPHelperProcess"},
	}}); err == nil || !strings.Contains(err.Error(), "cannot be enforced") {
		t.Fatalf("Open with unsandboxed MCP policy error = %v, want fail-closed policy error", err)
	}
}
