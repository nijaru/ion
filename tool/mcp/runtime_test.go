package mcp

import (
	"context"
	"errors"
	"fmt"
	"os"
	"testing"

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
	if _, ok, err := tools[0].(interface {
		ApprovalRequirement(string) (Requirement, bool, error)
	}).ApprovalRequirement(`{"msg":"hello"}`); err != nil || !ok {
		t.Fatalf("external tool approval = %v, %v; want required", err, ok)
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
