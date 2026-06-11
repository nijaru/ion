package agent

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/nijaru/ion/internal/core"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/instructions"
	"github.com/nijaru/ion/internal/skills"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers"
	"github.com/nijaru/ion/session"
)

// Backend implements core.Backend using the agent loop.
type Backend struct {
	cfg      *config.Config
	store    session.SessionStore
	sess     session.SessionHandle
	provider llm.Provider
	session  *Agent

	// Workdir is the current workspace directory.
	Workdir string

	// toolExecutor is the tool execution function. Set via SetToolExecutor.
	toolExecutor ToolExecutor
	// tools are the available tool definitions. Set via SetTools.
	tools []AgentTool
}

var _ core.Backend = (*Backend)(nil)
var _ core.ToolSummarizer = (*Backend)(nil)

// NewBackend creates a new agent backend.
func NewBackend() *Backend {
	return &Backend{}
}

// SetToolExecutor sets the tool executor for the backend.
func (b *Backend) SetToolExecutor(exec ToolExecutor) {
	b.toolExecutor = exec
	// Reset session to pick up new executor
	b.session = nil
}

// SetTools sets the available tool definitions for the backend.
func (b *Backend) SetTools(tools []AgentTool) {
	b.tools = tools
	// Reset session to pick up new tools
	b.session = nil
}

// Name returns the backend name.
func (b *Backend) Name() string {
	return "agent"
}

// Provider returns the provider name.
func (b *Backend) Provider() string {
	if b.cfg == nil {
		return ""
	}
	return strings.TrimSpace(b.cfg.Provider)
}

// Model returns the model name.
func (b *Backend) Model() string {
	if b.cfg == nil {
		return ""
	}
	return strings.TrimSpace(b.cfg.Model)
}

// ContextLimit returns the context window size.
func (b *Backend) ContextLimit() int {
	if b.cfg == nil {
		return 0
	}
	if b.cfg.ContextLimit > 0 {
		return b.cfg.ContextLimit
	}

	// Try to get from model metadata
	if meta, ok := llm.GetCachedMetadata(b.Provider(), b.Model()); ok {
		return meta.ContextLimit
	}

	return 0
}

// Bootstrap returns session bootstrap data.
func (b *Backend) Bootstrap() core.Bootstrap {
	return core.Bootstrap{
		Entries: nil,
		Status:  "ready",
	}
}

// Session returns the agent session.
func (b *Backend) Session() session.AgentSession {
	if b.session == nil {
		b.session = b.createSession()
	}
	return b.session
}

// SetStore sets the storage store.
func (b *Backend) SetStore(store session.SessionStore) {
	b.store = store
	if b.session != nil {
		b.session.SetStore(store)
	}
}

// SetSession sets the storage session.
func (b *Backend) SetSession(sess session.SessionHandle) {
	b.sess = sess
	if b.session != nil {
		b.session.SetSession(sess)
	}
}

// SetConfig sets the configuration.
func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
	// Reset session to pick up new config
	b.session = nil
}

// Compact implements core.Compactor.
// It generates a summary of the conversation and creates a CompactionSnapshot.
func (b *Backend) Compact(ctx context.Context) (bool, error) {
	if b.session == nil {
		return false, nil
	}

	b.session.mu.Lock()
	if b.session.contextTokens == 0 {
		b.session.mu.Unlock()
		return false, nil
	}

	// Get current messages from agent state
	messages := b.session.state.Messages
	if len(messages) == 0 {
		b.session.mu.Unlock()
		return false, nil
	}

	// Generate summary using LLM
	summary, err := b.generateSummary(ctx, messages)
	if err != nil {
		b.session.mu.Unlock()
		return false, fmt.Errorf("generate summary: %w", err)
	}

	// Update agent state with compacted messages
	b.session.state.Messages = []AgentMessage{
		{Role: "assistant", Content: summary},
	}
	b.session.resetContextTokens()
	b.session.mu.Unlock()

	return true, nil
}

// generateSummary generates a summary of the conversation using the LLM.
func (b *Backend) generateSummary(ctx context.Context, messages []AgentMessage) (string, error) {
	if b.provider == nil {
		return "", fmt.Errorf("no provider configured")
	}

	// Build summary prompt
	prompt := "Summarize the conversation so far. Include key decisions, file changes, and current state. Be concise but comprehensive."

	// Convert messages to LLM format
	llmMessages := make([]llm.Message, 0, len(messages)+1)
	llmMessages = append(llmMessages, llm.Message{
		Role:    llm.RoleSystem,
		Content: prompt,
	})
	for _, msg := range messages {
		llmMessages = append(llmMessages, llm.Message{
			Role:    llm.Role(msg.Role),
			Content: msg.Content,
		})
	}

	// Generate summary
	resp, err := b.provider.Generate(ctx, &llm.Request{
		Model:    b.Model(),
		Messages: llmMessages,
	})
	if err != nil {
		return "", fmt.Errorf("generate: %w", err)
	}

	return resp.TextContent(), nil
}

// createSession creates a new session adapter from the current app.
func (b *Backend) createSession() *Agent {
	if b.cfg == nil {
		return New(AgentConfig{
			ID: "default",
		})
	}

	// Create provider from config
	provider, err := providers.NewProviderFromConfig(b.cfg)
	if err != nil {
		// Return a basic session without streaming
		return New(AgentConfig{
			ID: b.sessionID(),
		})
	}
	b.provider = provider

	// Get model from config
	model := llm.Model{ID: b.Model()}
	if meta, ok := llm.GetCachedMetadata(b.Provider(), b.Model()); ok {
		model.ContextWindow = meta.ContextLimit
		model.CostPer1MIn = meta.InputPrice
		model.CostPer1MOut = meta.OutputPrice
	}

	// Create stream function
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return provider.Stream(ctx, req)
	}

	// Load workspace instruction layers if Workdir is set
	systemPrompt := ""
	workdir := b.Workdir
	if workdir == "" {
		if wd, err := os.Getwd(); err == nil {
			workdir = wd
		}
	}
	if workdir != "" {
		if insts, err := instructions.BuildInstructions("", workdir); err == nil {
			systemPrompt = insts
		}
	}

	// Load and format skills if 'read' tool is available
	hasRead := false
	for _, t := range b.tools {
		if t.Name == "read" {
			hasRead = true
			break
		}
	}
	if hasRead {
		var skillPaths []string
		if home, err := os.UserHomeDir(); err == nil && home != "" {
			skillPaths = append(skillPaths, filepath.Join(home, ".ion", "skills"))
		}
		if workdir != "" {
			skillPaths = append(skillPaths, filepath.Join(workdir, ".ion", "skills"))
		}
		if formattedSkills, err := skills.FormatSkillsForPrompt(skillPaths...); err == nil && formattedSkills != "" {
			if systemPrompt != "" {
				systemPrompt += "\n" + formattedSkills
			} else {
				systemPrompt = formattedSkills
			}
		}
	}

	// Create session adapter
	sessionCfg := AgentConfig{
		ID:           b.sessionID(),
		Model:        model,
		StreamFn:     streamFn,
		ToolExecutor: b.toolExecutor,
		Tools:        b.tools,
		SystemPrompt: systemPrompt,
		CompactFunc:  b.Compact,
	}

	adapter := New(sessionCfg)

	// Set store and session if available
	if b.store != nil {
		adapter.SetStore(b.store)
	}
	if b.sess != nil {
		adapter.SetSession(b.sess)
	}

	return adapter
}

// sessionID returns the session ID from config or default.
func (b *Backend) sessionID() string {
	return "default"
}

func (b *Backend) ToolSurface() core.ToolSurface {
	var names []string
	for _, t := range b.tools {
		names = append(names, t.Name)
	}

	activeMode := "coding"
	if b.cfg != nil {
		activeMode = b.cfg.ActiveToolMode()
	}

	var activeNames []string
	for _, t := range b.tools {
		if activeMode == "coding" {
			switch t.Name {
			case "find", "grep", "ls":
				continue
			}
		}
		activeNames = append(activeNames, t.Name)
	}

	envMode := "inherit"
	if b.cfg != nil {
		envMode = b.cfg.ToolEnvMode()
	}

	return core.ToolSurface{
		Count:       len(b.tools),
		Names:       names,
		ActiveNames: activeNames,
		Mode:        activeMode,
		Environment: envMode,
	}
}
