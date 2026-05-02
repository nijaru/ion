package canto

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/canto/runtime"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/subagents"
)

type SubagentTool struct {
	backend  *Backend
	personas []subagents.Persona
}

type subagentContextMode string

const (
	subagentContextSummary subagentContextMode = "summary"
	subagentContextFork    subagentContextMode = "fork"
	subagentContextNone    subagentContextMode = "none"
)

type subagentInput struct {
	Agent       string `json:"agent"`
	Task        string `json:"task"`
	Context     string `json:"context"`
	ContextMode string `json:"context_mode"`
}

type normalizedSubagentInput struct {
	Agent       string
	Task        string
	Context     string
	ContextMode subagentContextMode
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
					"description": "Optional concise context to pass with the task. Not allowed when context_mode is none.",
				},
				"context_mode": map[string]any{
					"type":        "string",
					"description": "How to seed the child context: summary passes the task plus concise selected context; fork snapshots the current provider-visible parent history plus the task; none sends only the task and persona prompt.",
					"enum": []string{
						string(subagentContextSummary),
						string(subagentContextFork),
						string(subagentContextNone),
					},
					"default": string(subagentContextSummary),
				},
			},
			"required": []string{"agent", "task"},
		},
	}
}

func (t *SubagentTool) Execute(ctx context.Context, args string) (string, error) {
	input, err := parseSubagentInput(args)
	if err != nil {
		return "", err
	}
	persona, ok := subagents.Find(t.personas, input.Agent)
	if !ok {
		return "", fmt.Errorf("unknown subagent persona %q", input.Agent)
	}

	childAgent, err := t.backend.newChildAgent(ctx, persona)
	if err != nil {
		return "", err
	}

	result, err := t.backend.runner.Delegate(
		ctx,
		t.backend.ID(),
		input.childSpec(childID(persona.Name), childAgent, persona),
	)
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

func parseSubagentInput(args string) (normalizedSubagentInput, error) {
	var input subagentInput
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return normalizedSubagentInput{}, err
	}

	normalized := normalizedSubagentInput{
		Agent:   strings.TrimSpace(input.Agent),
		Task:    strings.TrimSpace(input.Task),
		Context: strings.TrimSpace(input.Context),
	}
	if normalized.Task == "" {
		return normalizedSubagentInput{}, fmt.Errorf("task is required")
	}

	switch mode := subagentContextMode(strings.TrimSpace(input.ContextMode)); mode {
	case "":
		normalized.ContextMode = subagentContextSummary
	case subagentContextSummary, subagentContextFork:
		normalized.ContextMode = mode
	case subagentContextNone:
		if normalized.Context != "" {
			return normalizedSubagentInput{}, fmt.Errorf(
				"context must be empty when context_mode is %q",
				subagentContextNone,
			)
		}
		normalized.ContextMode = mode
	default:
		return normalizedSubagentInput{}, fmt.Errorf("unsupported context_mode %q", mode)
	}

	return normalized, nil
}

func (i normalizedSubagentInput) childSpec(
	id string,
	childAgent agent.Agent,
	persona subagents.Persona,
) runtime.ChildSpec {
	spec := runtime.ChildSpec{
		ID:      id,
		Agent:   childAgent,
		Mode:    csession.ChildModeHandoff,
		Task:    i.Task,
		Context: i.Context,
		Metadata: map[string]any{
			"context_mode": string(i.ContextMode),
			"persona":      persona.Name,
			"model_slot":   string(persona.ModelSlot),
		},
	}

	switch i.ContextMode {
	case subagentContextFork:
		spec.Mode = csession.ChildModeFork
		spec.InitialMessages = []llm.Message{subagentTaskMessage(i.Task, i.Context)}
	case subagentContextNone:
		spec.Mode = csession.ChildModeFresh
		spec.Context = ""
		spec.InitialMessages = []llm.Message{subagentTaskMessage(i.Task, "")}
	}

	return spec
}

func subagentTaskMessage(task, context string) llm.Message {
	parts := []string{"Task: " + task}
	if context != "" {
		parts = append(parts, "Context: "+context)
	}
	return llm.Message{
		Role:    llm.RoleUser,
		Content: strings.Join(parts, "\n"),
	}
}

func (b *Backend) newChildAgent(
	ctx context.Context,
	persona subagents.Persona,
) (agent.Agent, error) {
	cfg := b.cfg
	if cfg == nil {
		return nil, fmt.Errorf("subagent config is not initialized")
	}
	preset := registry.PresetPrimary
	if persona.ModelSlot == subagents.ModelSlotFast && strings.TrimSpace(cfg.FastModel) != "" {
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

	instructions := strings.TrimSpace(
		b.agent.Instructions(),
	) + "\n\n## Subagent Persona: " + persona.Name + "\n" + persona.Prompt
	requestProcessors := []prompt.RequestProcessor{
		reasoningEffortProcessor(runtimeCfg),
		reflexionProcessor(),
	}
	return agent.New(persona.Name, instructions, runtimeCfg.Model, b.llm, scopedTools,
		agent.WithHooks(policyHook(b)),
		agent.WithRequestProcessors(requestProcessors...),
	), nil
}

func childID(name string) string {
	return strings.TrimSpace(name) + "-" + fmt.Sprint(time.Now().UnixNano())
}
