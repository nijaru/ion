package mcp

import (
	"context"
	"errors"
	"net"
	"testing"

	sdkmcp "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/tool"
)

// echoTool is a minimal tool.Tool for testing.
type echoTool struct{ name string }

func (e *echoTool) Spec() llm.Spec {
	return llm.Spec{
		Name:        e.name,
		Description: "Echoes the input.",
		Parameters: map[string]any{
			"type":       "object",
			"properties": map[string]any{"msg": map[string]any{"type": "string"}},
			"required":   []string{"msg"},
		},
	}
}

func (e *echoTool) Execute(_ context.Context, _ string) (string, error) {
	return "echo: hello", nil
}

func newTestServer() (*Server, *tool.Registry) {
	reg := tool.NewRegistry()
	reg.Register(&echoTool{name: "echo"})
	return NewServer(reg, "test", "0.1"), reg
}

func connectTestSession(t *testing.T, srv *Server) (*sdkmcp.ClientSession, context.CancelFunc) {
	t.Helper()

	serverTransport, clientTransport := sdkmcp.NewInMemoryTransports()
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() {
		done <- srv.Run(ctx, serverTransport)
	}()
	t.Cleanup(func() {
		cancel()
		err := <-done
		if err != nil && !errors.Is(err, context.Canceled) {
			t.Fatalf("server run: %v", err)
		}
	})

	client := sdkmcp.NewClient(&sdkmcp.Implementation{Name: "test-client", Version: "0.1"}, nil)
	session, err := client.Connect(ctx, clientTransport, nil)
	if err != nil {
		t.Fatalf("client connect: %v", err)
	}
	t.Cleanup(func() {
		_ = session.Close()
	})
	return session, cancel
}

func TestServer_ToolsList(t *testing.T) {
	srv, _ := newTestServer()
	session, _ := connectTestSession(t, srv)

	result, err := session.ListTools(t.Context(), nil)
	if err != nil {
		t.Fatalf("ListTools: %v", err)
	}
	if len(result.Tools) != 1 {
		t.Fatalf("len(tools) = %d, want 1", len(result.Tools))
	}
	if result.Tools[0].Name != "echo" {
		t.Fatalf("tool name = %q, want echo", result.Tools[0].Name)
	}
	schema, ok := result.Tools[0].InputSchema.(map[string]any)
	if !ok {
		t.Fatalf("input schema type = %T, want map[string]any", result.Tools[0].InputSchema)
	}
	if schema["type"] != "object" {
		t.Fatalf("schema.type = %v, want object", schema["type"])
	}
}

func TestServer_ToolsCall(t *testing.T) {
	srv, _ := newTestServer()
	session, _ := connectTestSession(t, srv)

	result, err := session.CallTool(t.Context(), &sdkmcp.CallToolParams{
		Name:      "echo",
		Arguments: map[string]any{"msg": "hello"},
	})
	if err != nil {
		t.Fatalf("CallTool: %v", err)
	}
	if result.IsError {
		t.Fatal("CallTool returned IsError=true")
	}
	if len(result.Content) != 1 {
		t.Fatalf("len(content) = %d, want 1", len(result.Content))
	}
	text, ok := result.Content[0].(*sdkmcp.TextContent)
	if !ok {
		t.Fatalf("content type = %T, want *mcp.TextContent", result.Content[0])
	}
	if text.Text != "echo: hello" {
		t.Fatalf("text = %q, want %q", text.Text, "echo: hello")
	}
}

func TestServer_UnknownToolReturnsProtocolError(t *testing.T) {
	srv, _ := newTestServer()
	session, _ := connectTestSession(t, srv)

	if _, err := session.CallTool(t.Context(), &sdkmcp.CallToolParams{
		Name:      "no-such-tool",
		Arguments: map[string]any{},
	}); err == nil {
		t.Fatal("CallTool should fail for unknown tool")
	}
}

func TestServer_ServeOverIOTransport(t *testing.T) {
	srv, _ := newTestServer()
	serverConn, clientConn := net.Pipe()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- srv.Serve(ctx, serverConn, serverConn)
	}()
	defer func() {
		_ = serverConn.Close()
		err := <-done
		if err != nil && !errors.Is(err, context.Canceled) {
			t.Fatalf("Serve: %v", err)
		}
	}()

	client := sdkmcp.NewClient(&sdkmcp.Implementation{Name: "test-client", Version: "0.1"}, nil)
	session, err := client.Connect(t.Context(), &sdkmcp.IOTransport{
		Reader: clientConn,
		Writer: clientConn,
	}, nil)
	if err != nil {
		t.Fatalf("client connect over io transport: %v", err)
	}
	defer func() {
		_ = session.Close()
		_ = clientConn.Close()
	}()

	result, err := session.ListTools(t.Context(), nil)
	if err != nil {
		t.Fatalf("ListTools over io transport: %v", err)
	}
	if len(result.Tools) != 1 || result.Tools[0].Name != "echo" {
		t.Fatalf("unexpected tools result: %#v", result.Tools)
	}
}
