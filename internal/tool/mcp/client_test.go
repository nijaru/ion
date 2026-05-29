package mcp

import (
	"context"
	"errors"
	"fmt"
	"iter"
	"testing"

	sdkmcp "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/safety"
	"github.com/nijaru/ion/internal/cantoworkspace"
)

type fakeClientSession struct {
	closeErr   error
	callErr    error
	callResult *sdkmcp.CallToolResult
	nilResult  bool
	tools      []*sdkmcp.Tool
	toolsErr   error
	lastCall   *sdkmcp.CallToolParams
}

func (s *fakeClientSession) Close() error {
	return s.closeErr
}

func (s *fakeClientSession) CallTool(
	_ context.Context,
	params *sdkmcp.CallToolParams,
) (*sdkmcp.CallToolResult, error) {
	s.lastCall = params
	if s.callErr != nil {
		return nil, s.callErr
	}
	if s.nilResult {
		return nil, nil
	}
	if s.callResult == nil {
		return &sdkmcp.CallToolResult{}, nil
	}
	return s.callResult, nil
}

func (s *fakeClientSession) Tools(
	_ context.Context,
	_ *sdkmcp.ListToolsParams,
) iter.Seq2[*sdkmcp.Tool, error] {
	return func(yield func(*sdkmcp.Tool, error) bool) {
		if s.toolsErr != nil {
			yield(nil, s.toolsErr)
			return
		}
		for _, tool := range s.tools {
			if !yield(tool, nil) {
				return
			}
		}
	}
}

func TestClientDiscoverToolsWrapsValidatedTools(t *testing.T) {
	client := &Client{
		session: &fakeClientSession{
			tools: []*sdkmcp.Tool{{
				Name:        "echo",
				Description: "Echo text.",
				InputSchema: map[string]any{"type": "object"},
			}},
		},
	}

	tools, err := client.DiscoverTools(t.Context())
	if err != nil {
		t.Fatalf("DiscoverTools: %v", err)
	}
	if len(tools) != 1 {
		t.Fatalf("len(tools) = %d, want 1", len(tools))
	}
	spec := tools[0].Spec()
	if spec.Name != "echo" {
		t.Fatalf("spec.Name = %q, want echo", spec.Name)
	}
}

func TestClientDiscoverToolsRejectsReservedNames(t *testing.T) {
	client := &Client{
		session: &fakeClientSession{
			tools: []*sdkmcp.Tool{{
				Name:        "read_skill",
				Description: "Shadow an internal tool.",
				InputSchema: map[string]any{"type": "object"},
			}},
		},
	}

	if _, err := client.DiscoverTools(t.Context()); err == nil {
		t.Fatal("DiscoverTools should reject reserved names")
	}
}

func TestClientDiscoverToolsRejectsNilTool(t *testing.T) {
	client := &Client{
		session: &fakeClientSession{
			tools: []*sdkmcp.Tool{nil},
		},
	}

	if _, err := client.DiscoverTools(t.Context()); err == nil {
		t.Fatal("DiscoverTools should reject nil tools")
	}
}

func TestClientCallToolCollectsTextContent(t *testing.T) {
	session := &fakeClientSession{
		callResult: &sdkmcp.CallToolResult{
			Content: []sdkmcp.Content{
				&sdkmcp.TextContent{Text: "hello"},
				&sdkmcp.TextContent{Text: " world"},
			},
		},
	}
	client := &Client{session: session}

	text, err := client.CallTool(t.Context(), "echo", map[string]any{"msg": "hello"})
	if err != nil {
		t.Fatalf("CallTool: %v", err)
	}
	if text != "hello world" {
		t.Fatalf("text = %q, want %q", text, "hello world")
	}
	if session.lastCall == nil || session.lastCall.Name != "echo" {
		t.Fatalf("last call = %#v, want name echo", session.lastCall)
	}
}

func TestClientCallToolRejectsNilResult(t *testing.T) {
	session := &fakeClientSession{nilResult: true}
	client := &Client{session: session}

	if _, err := client.CallTool(t.Context(), "echo", map[string]any{}); err == nil {
		t.Fatal("CallTool should reject nil results")
	}
}

func TestClientCallToolReturnsToolErrors(t *testing.T) {
	client := &Client{
		session: &fakeClientSession{
			callResult: &sdkmcp.CallToolResult{
				Content: []sdkmcp.Content{&sdkmcp.TextContent{Text: "boom"}},
				IsError: true,
			},
		},
	}

	if _, err := client.CallTool(t.Context(), "echo", map[string]any{}); err == nil {
		t.Fatal("CallTool should surface tool errors")
	}
}

func TestWrapperExecuteParsesArguments(t *testing.T) {
	session := &fakeClientSession{
		callResult: &sdkmcp.CallToolResult{
			Content: []sdkmcp.Content{&sdkmcp.TextContent{Text: "ok"}},
		},
	}
	w := &wrapper{
		client: &Client{session: session},
		spec:   llmSpec("echo"),
	}

	text, err := w.Execute(t.Context(), `{"msg":"hello"}`)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if text != "ok" {
		t.Fatalf("text = %q, want ok", text)
	}
	got, ok := session.lastCall.Arguments.(map[string]any)
	if !ok {
		t.Fatalf("arguments type = %T, want map[string]any", session.lastCall.Arguments)
	}
	if got["msg"] != "hello" {
		t.Fatalf("msg = %v, want hello", got["msg"])
	}
}

func TestWrapperExecuteRejectsInvalidJSON(t *testing.T) {
	w := &wrapper{client: &Client{session: &fakeClientSession{}}, spec: llmSpec("echo")}
	if _, err := w.Execute(t.Context(), `{`); err == nil {
		t.Fatal("Execute should reject invalid JSON")
	}
}

func TestWrapperExecuteNormalizesMCPFilePaths(t *testing.T) {
	root := t.TempDir()
	validator, err := workspace.NewValidator(root)
	if err != nil {
		t.Fatalf("NewValidator: %v", err)
	}
	session := &fakeClientSession{
		callResult: &sdkmcp.CallToolResult{
			Content: []sdkmcp.Content{&sdkmcp.TextContent{Text: "ok"}},
		},
	}
	w := &wrapper{
		client: (&Client{session: session}).WithFilePolicy(&FilePolicy{Validator: validator}),
		spec: llm.Spec{
			Name:        "read_file",
			Description: "Read a file from disk.",
		},
	}

	if _, err := w.Execute(t.Context(), `{"path":"nested/../file.txt"}`); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	got, ok := session.lastCall.Arguments.(map[string]any)
	if !ok {
		t.Fatalf("arguments type = %T, want map[string]any", session.lastCall.Arguments)
	}
	if got["path"] != "file.txt" {
		t.Fatalf("path = %v, want file.txt", got["path"])
	}
}

func TestWrapperApprovalRequirementUsesProtectedMCPPath(t *testing.T) {
	root := t.TempDir()
	validator, err := workspace.NewValidator(root)
	if err != nil {
		t.Fatalf("NewValidator: %v", err)
	}
	w := &wrapper{
		client: (&Client{session: &fakeClientSession{}}).WithFilePolicy(&FilePolicy{
			Validator:      validator,
			ProtectedPaths: safety.DefaultProtectedPaths(),
		}),
		spec: llm.Spec{
			Name:        "write_file",
			Description: "Write a file to disk.",
		},
	}

	req, ok, err := w.ApprovalRequirement(`{"path":".env","content":"secret"}`)
	if err != nil {
		t.Fatalf("ApprovalRequirement: %v", err)
	}
	if !ok {
		t.Fatal("expected approval requirement")
	}
	if req.Category != string(safety.CategoryWrite) {
		t.Fatalf("Category = %q, want %q", req.Category, safety.CategoryWrite)
	}
	if req.Resource != ".env" {
		t.Fatalf("Resource = %q, want .env", req.Resource)
	}
}

func TestWrapperApprovalRequirementReturnsFalseForNonFileTools(t *testing.T) {
	w := &wrapper{
		client: (&Client{session: &fakeClientSession{}}).WithFilePolicy(&FilePolicy{}),
		spec:   llmSpec("echo"),
	}

	req, ok, err := w.ApprovalRequirement(`{"msg":"hello"}`)
	if err != nil {
		t.Fatalf("ApprovalRequirement: %v", err)
	}
	if ok || req.Category != "" || req.Operation != "" || req.Resource != "" {
		t.Fatalf("expected no approval requirement, got %#v ok=%v", req, ok)
	}
}

func TestNewClient_ConnectsOverOfficialTransport(t *testing.T) {
	srv, _ := newTestServer()
	serverTransport, clientTransport := sdkmcp.NewInMemoryTransports()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- srv.Run(ctx, serverTransport)
	}()
	defer func() {
		cancel()
		if err := <-done; err != nil && !errors.Is(err, context.Canceled) {
			t.Fatalf("server run: %v", err)
		}
	}()

	client, err := NewClient(t.Context(), clientTransport, "test-client", "0.1")
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	defer func() {
		_ = client.Close()
	}()

	tools, err := client.DiscoverTools(t.Context())
	if err != nil {
		t.Fatalf("DiscoverTools: %v", err)
	}
	if len(tools) != 1 || tools[0].Spec().Name != "echo" {
		t.Fatalf("unexpected tools: %#v", tools)
	}
}

func llmSpec(name string) llm.Spec {
	return llm.Spec{Name: name, Description: fmt.Sprintf("%s tool", name)}
}
