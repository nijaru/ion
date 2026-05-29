// Package agenttools bridges between the agent loop and Ion's coding tools.
// It lives in its own package to avoid import cycles between agent/ and tools/.
package agenttools

import (
	"context"
	"encoding/json"
	"fmt"

	ionllm "github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/agent"
)

// ExecutorFromRegistry creates an agent.ToolExecutor that dispatches
// tool calls to a Canto tool.Registry. Each AgentToolCall is routed to
// the matching registered tool by name.
func ExecutorFromRegistry(registry *tool.Registry) agent.ToolExecutor {
	return func(ctx context.Context, tc agent.AgentToolCall) (agent.AgentToolResult, error) {
		t, ok := registry.Get(tc.Name)
		if !ok {
			return agent.AgentToolResult{
				Content: []ionllm.ContentPart{
					{Type: "text", Text: fmt.Sprintf("Unknown tool: %s", tc.Name)},
				},
				IsError: true,
			}, nil
		}

		argsJSON, err := json.Marshal(tc.Arguments)
		if err != nil {
			return agent.AgentToolResult{
				Content: []ionllm.ContentPart{
					{Type: "text", Text: fmt.Sprintf("Failed to marshal tool arguments: %v", err)},
				},
				IsError: true,
			}, nil
		}

		// Try ContentTool first for richer output (images, etc.)
		if ct, ok := t.(tool.ContentTool); ok {
			parts, execErr := ct.ExecuteContent(ctx, string(argsJSON))
			if execErr != nil {
				return agent.AgentToolResult{
					Content: []ionllm.ContentPart{
						{Type: "text", Text: execErr.Error()},
					},
					IsError: true,
				}, nil
			}
			// Convert canto ContentParts to Ion ContentParts
			ionParts := make([]ionllm.ContentPart, len(parts))
			for i, p := range parts {
				ionParts[i] = ionllm.ContentPart{Type: ionllm.ContentPartType(p.Type), Text: p.Text}
			}
			return agent.AgentToolResult{Content: ionParts}, nil
		}

		// Fall back to plain text Execute
		result, execErr := t.Execute(ctx, string(argsJSON))
		if execErr != nil {
			return agent.AgentToolResult{
				Content: []ionllm.ContentPart{
					{Type: "text", Text: execErr.Error()},
				},
				IsError: true,
			}, nil
		}
		return agent.AgentToolResult{
			Content: []ionllm.ContentPart{
				{Type: "text", Text: result},
			},
		}, nil
	}
}

// ToolsFromRegistry returns the agent.AgentTool definitions for all
// tools registered in the Canto registry, suitable for sending in LLM
// tool-spec requests.
func ToolsFromRegistry(registry *tool.Registry) []agent.AgentTool {
	var tools []agent.AgentTool
	for _, entry := range registry.Entries() {
		at := agent.AgentTool{
			Name:        entry.Spec.Name,
			Description: entry.Spec.Description,
			Parameters:  entry.Spec.Parameters,
		}
		// Mark read-only tools for parallel execution
		if _, ok := entry.Tool.(tool.ContentTool); ok {
			at.ReadOnly = true
			at.Parallel = true
		}
		tools = append(tools, at)
	}
	return tools
}
