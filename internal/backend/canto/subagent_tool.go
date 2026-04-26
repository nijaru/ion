package canto

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/runtime"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/subagents"
)

type SubagentTool struct {
	backend  *Backend
	personas []subagents.Persona
}

func NewSubagentTool(backend *Backend, personas []subagents.Persona) *SubagentTool {
	return &SubagentTool{backend: backend, personas: personas}
}

func (t *SubagentTool) Spec() llm.Spec {
	agentNames := make([]string, 0, len(t.personas))
	for _, persona := range t.personas {
		agentNames = append(agentNames, persona.Name)
	}
	return llm.Spec{
		Name:        "subagent",
		Description: "Delegate a focused task to a scoped child coding agent and return its summary.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"agent": map[string]any{
					"type":        "string",
					"description": "Subagent persona to run.",
					"enum":        agentNames,
				},
				"task": map[string]any{
					"type":        "string",
					"description": "Concrete task for the child agent.",
				},
				"context": map[string]any{
					"type":        "string",
					"description": "Optional concise context to pass with the task.",
				},
			},
			"required": []string{"agent", "task"},
		},
	}
}

func (t *SubagentTool) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Agent   string `json:"agent"`
		Task    string `json:"task"`
		Context string `json:"context"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	persona, ok := subagents.Find(t.personas, input.Agent)
	if !ok {
		return "", fmt.Errorf("unknown subagent persona %q", input.Agent)
	}
	if strings.TrimSpace(input.Task) == "" {
		return "", fmt.Errorf("task is required")
	}

	childAgent, err := t.backend.newChildAgent(ctx, persona)
	if err != nil {
		return "", err
	}

	result, err := t.backend.runner.Delegate(ctx, t.backend.ID(), runtime.ChildSpec{
		ID:      childID(persona.Name),
		Agent:   childAgent,
		Mode:    csession.ChildModeHandoff,
		Task:    strings.TrimSpace(input.Task),
		Context: strings.TrimSpace(input.Context),
		Metadata: map[string]any{
			"persona":    persona.Name,
			"model_slot": string(persona.ModelSlot),
		},
	})
	if err != nil {
		return "", err
	}
	if result.Err != nil {
		return "", result.Err
	}
	if result.Status != csession.ChildStatusCompleted {
		return "", fmt.Errorf("subagent %s ended with status %s", persona.Name, result.Status)
	}
	return strings.TrimSpace(result.Summary), nil
}

func (b *Backend) newChildAgent(ctx context.Context, persona subagents.Persona) (agent.Agent, error) {
	cfg := b.cfg
	if cfg == nil {
		return nil, fmt.Errorf("subagent config is not initialized")
	}
	preset := registry.PresetPrimary
	if persona.ModelSlot == subagents.ModelSlotFast {
		preset = registry.PresetFast
	}
	runtimeCfg, err := registry.ResolveRuntimeConfig(ctx, cfg, preset)
	if err != nil {
		return nil, err
	}
	if strings.TrimSpace(runtimeCfg.Model) == "" {
		return nil, fmt.Errorf("subagent %s resolved empty model", persona.Name)
	}

	scopedTools, err := b.tools.Subset(persona.Tools...)
	if err != nil {
		return nil, err
	}

	instructions := strings.TrimSpace(b.agent.Instructions()) + "\n\n## Subagent Persona: " + persona.Name + "\n" + persona.Prompt
	return agent.New(persona.Name, instructions, runtimeCfg.Model, b.llm, scopedTools,
		agent.WithHooks(policyHook(b)),
		agent.WithRequestProcessors(reasoningEffortProcessor(runtimeCfg), reflexionProcessor()),
	), nil
}

func childID(name string) string {
	return strings.TrimSpace(name) + "-" + fmt.Sprint(time.Now().UnixNano())
}
