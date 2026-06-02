package mcp

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/go-json-experiment/json"
	sdkmcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/nijaru/ion/tool"
)

// Server exposes a tool.Registry through the official MCP Go SDK while
// preserving Canto-owned registry and validation semantics.
type Server struct {
	reg     *tool.Registry
	name    string
	version string
}

// NewServer creates a new MCP server backed by the given registry.
func NewServer(reg *tool.Registry, name, version string) *Server {
	return &Server{reg: reg, name: name, version: version}
}

// Run serves the registry over the provided MCP transport.
func (s *Server) Run(ctx context.Context, transport sdkmcp.Transport) error {
	server := sdkmcp.NewServer(&sdkmcp.Implementation{Name: s.name, Version: s.version}, nil)

	if s.reg != nil {
		for _, name := range s.reg.Names() {
			toolImpl, ok := s.reg.Get(name)
			if !ok {
				continue
			}
			spec := toolImpl.Spec()
			server.AddTool(&sdkmcp.Tool{
				Name:        spec.Name,
				Description: spec.Description,
				InputSchema: normalizeSchema(spec.Parameters),
			}, func(ctx context.Context, req *sdkmcp.CallToolRequest) (*sdkmcp.CallToolResult, error) {
				output, err := toolImpl.Execute(ctx, string(req.Params.Arguments))
				if err != nil {
					return &sdkmcp.CallToolResult{
						Content: []sdkmcp.Content{&sdkmcp.TextContent{Text: err.Error()}},
						IsError: true,
					}, nil
				}
				return &sdkmcp.CallToolResult{
					Content: []sdkmcp.Content{&sdkmcp.TextContent{Text: output}},
				}, nil
			})
		}
	}

	err := server.Run(ctx, transport)
	if ctx.Err() != nil || isCleanShutdown(err) {
		return ctx.Err()
	}
	if err != nil {
		return fmt.Errorf("mcp run: %w", err)
	}
	return nil
}

// Serve adapts separate reader/writer streams into an MCP IO transport.
func (s *Server) Serve(ctx context.Context, r io.Reader, w io.Writer) error {
	return s.Run(ctx, &sdkmcp.IOTransport{
		Reader: asReadCloser(r),
		Writer: asWriteCloser(w),
	})
}

func isCleanShutdown(err error) bool {
	if err == nil {
		return true
	}
	return errors.Is(err, io.EOF) ||
		errors.Is(err, sdkmcp.ErrConnectionClosed) ||
		strings.Contains(err.Error(), "server is closing: EOF")
}

func asReadCloser(r io.Reader) io.ReadCloser {
	if rc, ok := r.(io.ReadCloser); ok {
		return rc
	}
	return io.NopCloser(r)
}

type nopWriteCloser struct {
	io.Writer
}

func (nopWriteCloser) Close() error { return nil }

func asWriteCloser(w io.Writer) io.WriteCloser {
	if wc, ok := w.(io.WriteCloser); ok {
		return wc
	}
	return nopWriteCloser{Writer: w}
}

// normalizeSchema converts a Spec.Parameters (any JSON-serializable value)
// to a map[string]any suitable for the MCP inputSchema field.
func normalizeSchema(params any) map[string]any {
	base := map[string]any{"type": "object"}
	if params == nil {
		return base
	}
	raw, err := json.Marshal(params)
	if err != nil {
		return base
	}
	var m map[string]any
	if err := json.Unmarshal(raw, &m); err != nil {
		return base
	}
	if _, hasType := m["type"]; !hasType {
		m["type"] = "object"
	}
	return m
}
