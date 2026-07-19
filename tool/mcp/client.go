package mcp

import (
	"context"
	"fmt"
	"iter"
	"os/exec"

	"github.com/go-json-experiment/json"
	sdkmcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/tool"
)

type clientSession interface {
	Close() error
	CallTool(context.Context, *sdkmcp.CallToolParams) (*sdkmcp.CallToolResult, error)
	Tools(context.Context, *sdkmcp.ListToolsParams) iter.Seq2[*sdkmcp.Tool, error]
}

// Client represents an MCP client session wrapped with Ion validation and
// tool-registry adaptation.
type Client struct {
	session    clientSession
	filePolicy *FilePolicy
	identity   string
}

// NewClient connects to an MCP server over the provided official SDK transport.
func NewClient(
	ctx context.Context,
	transport sdkmcp.Transport,
	name string,
	version string,
) (*Client, error) {
	if name == "" {
		name = "ion"
	}
	if version == "" {
		version = "0.0.1"
	}

	client := sdkmcp.NewClient(&sdkmcp.Implementation{Name: name, Version: version}, nil)
	session, err := client.Connect(ctx, transport, nil)
	if err != nil {
		return nil, fmt.Errorf("mcp connect: %w", err)
	}
	return &Client{session: session}, nil
}

// NewStdioClient starts an MCP server process and connects via stdio.
func NewStdioClient(ctx context.Context, command string, args ...string) (*Client, error) {
	return NewClient(
		ctx,
		&sdkmcp.CommandTransport{Command: exec.CommandContext(ctx, command, args...)},
		"ion",
		"0.0.1",
	)
}

// Close shuts down the client and the underlying process.
func (c *Client) Close() error {
	if c == nil || c.session == nil {
		return nil
	}
	return c.session.Close()
}

// WithFilePolicy configures workspace/sensitive-path handling for MCP tools
// that behave like file operations.
func (c *Client) WithFilePolicy(policy *FilePolicy) *Client {
	if c == nil {
		return nil
	}
	c.filePolicy = policy
	return c
}

// WithIdentity binds discovered tool calls to the configuration of the
// long-lived MCP server process without persisting its environment values.
func (c *Client) WithIdentity(identity string) *Client {
	if c == nil {
		return nil
	}
	c.identity = identity
	return c
}

// DiscoverTools fetches available tools from the MCP server and returns them
// as tool.Tool values that can be registered in a tool.Registry.
func (c *Client) DiscoverTools(ctx context.Context) ([]tool.Tool, error) {
	return c.discoverTools(ctx, "")
}

func (c *Client) discoverTools(ctx context.Context, namespace string) ([]tool.Tool, error) {
	if c == nil || c.session == nil {
		return nil, fmt.Errorf("mcp: nil client session")
	}

	var tools []tool.Tool
	for remoteTool, err := range c.session.Tools(ctx, nil) {
		if err != nil {
			return nil, fmt.Errorf("mcp tools/list: %w", err)
		}
		if remoteTool == nil {
			return nil, fmt.Errorf("mcp tools/list: nil tool")
		}
		remoteName := remoteTool.Name
		remoteSpec := llm.Spec{
			Name:        remoteName,
			Description: remoteTool.Description,
			Parameters:  normalizeSchema(remoteTool.InputSchema),
		}
		if err := Validate(remoteSpec); err != nil {
			return nil, err
		}
		effectiveName := remoteName
		if namespace != "" {
			effectiveName = namespacedToolName(namespace, remoteName)
		}
		spec := llm.Spec{
			Name:        effectiveName,
			Description: remoteSpec.Description,
			Parameters:  remoteSpec.Parameters,
		}
		if err := Validate(spec); err != nil {
			return nil, err
		}
		identity := c.identity
		if identity == "" {
			identity = "mcp:client:" + remoteName
			if namespace != "" {
				identity = "mcp:server:" + namespace
			}
		}
		tools = append(tools, &wrapper{
			client:      c,
			spec:        spec,
			remoteName:  remoteName,
			mcpIdentity: identity,
		})
	}
	return tools, nil
}

// CallTool executes a tool on the MCP server and returns its text output.
func (c *Client) CallTool(
	ctx context.Context,
	name string,
	arguments map[string]any,
) (string, error) {
	if c == nil || c.session == nil {
		return "", fmt.Errorf("mcp: nil client session")
	}

	resp, err := c.session.CallTool(ctx, &sdkmcp.CallToolParams{
		Name:      name,
		Arguments: arguments,
	})
	if err != nil {
		return "", fmt.Errorf("mcp tools/call %q: %w", name, err)
	}
	if resp == nil {
		return "", fmt.Errorf("mcp tools/call %q: nil result", name)
	}

	var text string
	for _, block := range resp.Content {
		if content, ok := block.(*sdkmcp.TextContent); ok {
			text += content.Text
		}
	}
	if resp.IsError {
		return "", fmt.Errorf("mcp tool %q error: %s", name, text)
	}
	return text, nil
}

// wrapper implements the tool.Tool interface for an MCP tool.
type wrapper struct {
	client      *Client
	spec        llm.Spec
	remoteName  string
	mcpIdentity string
}

func (w *wrapper) Spec() llm.Spec {
	return w.spec
}

func (w *wrapper) Metadata() tool.Metadata {
	return tool.Metadata{
		Category:    "mcp",
		Concurrency: tool.Unknown,
	}
}

func (w *wrapper) Execute(ctx context.Context, args string) (string, error) {
	var parsedArgs map[string]any
	if err := json.Unmarshal([]byte(args), &parsedArgs); err != nil {
		return "", fmt.Errorf("failed to parse arguments: %w", err)
	}
	if w.client != nil && w.client.filePolicy != nil {
		var err error
		parsedArgs, _, err = w.client.filePolicy.normalizeArguments(w.spec, parsedArgs)
		if err != nil {
			return "", err
		}
	}
	remoteName := w.remoteName
	if remoteName == "" {
		remoteName = w.spec.Name
	}
	return w.client.CallTool(ctx, remoteName, parsedArgs)
}

func (w *wrapper) ApprovalRequirement(args string) (Requirement, bool, error) {
	if w == nil || w.client == nil {
		return Requirement{}, false, fmt.Errorf("mcp: nil tool wrapper")
	}
	var parsedArgs map[string]any
	if err := json.Unmarshal([]byte(args), &parsedArgs); err != nil {
		return Requirement{}, false, fmt.Errorf("failed to parse arguments: %w", err)
	}
	var (
		requirement Requirement
		required    bool
	)
	if w.client.filePolicy != nil {
		var err error
		requirement, required, err = w.client.filePolicy.approvalRequirement(w.spec, parsedArgs)
		if err != nil {
			return Requirement{}, false, err
		}
	}
	if !required {
		resource := w.remoteName
		if resource == "" {
			resource = w.spec.Name
		}
		requirement = Requirement{
			Category:  "mcp",
			Operation: w.spec.Name,
			Resource:  resource,
		}
	}
	if requirement.Category == "" {
		requirement.Category = "mcp"
	}
	if requirement.Operation == "" {
		requirement.Operation = w.spec.Name
	}
	if requirement.Resource == "" {
		requirement.Resource = w.remoteName
		if requirement.Resource == "" {
			requirement.Resource = w.spec.Name
		}
	}
	requirement.MCPIdentity = w.mcpIdentity
	if requirement.MCPIdentity == "" {
		identityName := w.remoteName
		if identityName == "" {
			identityName = w.spec.Name
		}
		requirement.MCPIdentity = "mcp:client:" + identityName
	}
	return requirement, true, nil
}
