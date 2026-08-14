package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/nijaru/ion/config"
)

type ConfigField struct {
	Value  any    `json:"value"`
	Source string `json:"source"`
}

type EffectiveConfigExplanation struct {
	WorkspaceRoot string                 `json:"workspace_root"`
	TrustStatus   string                 `json:"trust_status"`
	Fields        map[string]ConfigField `json:"fields"`
}

func configCommandUsage() string {
	return `Usage: ion config <subcommand> [flags]

Subcommands:
  explain [--json]    Explain effective configuration and provenance
`
}

func pathExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func runConfigCommand(args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New(configCommandUsage())
	}
	switch args[0] {
	case "explain":
		return runConfigExplain(args[1:], stdout)
	default:
		return errors.New(configCommandUsage())
	}
}

func runConfigExplain(args []string, stdout io.Writer) error {
	flagSet := flag.NewFlagSet("ion config explain", flag.ContinueOnError)
	flagSet.SetOutput(io.Discard)
	jsonOutput := flagSet.Bool("json", false, "Emit structured JSON explanation")
	if err := flagSet.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			_, err = fmt.Fprintln(stdout, "Usage: ion config explain [--json]")
			return err
		}
		return err
	}

	workdir, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("resolve working directory: %w", err)
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("load configuration: %w", err)
	}

	trusted, _ := config.IsProjectTrusted(workdir)
	trustState := "untrusted"
	if trusted {
		trustState = "trusted"
	}

	configPath, _ := config.DefaultConfigPath()

	fields := make(map[string]ConfigField)

	// Provider
	provider := cfg.Provider
	providerSrc := "default"
	if env := os.Getenv("ION_PROVIDER"); strings.TrimSpace(env) != "" {
		provider = env
		providerSrc = "env: ION_PROVIDER"
	} else if configPath != "" && pathExists(configPath) && cfg.Provider != "" {
		providerSrc = "config: " + configPath
	}
	if provider == "" {
		provider = "openrouter"
	}
	fields["provider"] = ConfigField{Value: provider, Source: providerSrc}

	// Model
	model := cfg.Model
	modelSrc := "default"
	if env := os.Getenv("ION_MODEL"); strings.TrimSpace(env) != "" {
		model = env
		modelSrc = "env: ION_MODEL"
	} else if configPath != "" && pathExists(configPath) && cfg.Model != "" {
		modelSrc = "config: " + configPath
	}
	if model == "" {
		model = "default"
	}
	fields["model"] = ConfigField{Value: model, Source: modelSrc}

	// Reasoning Effort / Thinking
	thinking := cfg.ReasoningEffort
	thinkingSrc := "default"
	if env := os.Getenv("ION_THINKING"); strings.TrimSpace(env) != "" {
		thinking = env
		thinkingSrc = "env: ION_THINKING"
	} else if configPath != "" && pathExists(configPath) && cfg.ReasoningEffort != "" {
		thinkingSrc = "config: " + configPath
	}
	if thinking == "" {
		thinking = config.DefaultReasoningEffort
	}
	fields["reasoning_effort"] = ConfigField{Value: thinking, Source: thinkingSrc}

	// Trust Mode
	trustMode := cfg.TrustMode
	trustModeSrc := "default"
	if env := os.Getenv("ION_TRUST_MODE"); strings.TrimSpace(env) != "" {
		trustMode = env
		trustModeSrc = "env: ION_TRUST_MODE"
	} else if configPath != "" && pathExists(configPath) && cfg.TrustMode != "" {
		trustModeSrc = "config: " + configPath
	}
	if trustMode == "" {
		trustMode = "trusted"
	}
	fields["trust_mode"] = ConfigField{Value: trustMode, Source: trustModeSrc}

	// Tool Mode
	toolMode := cfg.ToolMode
	toolModeSrc := "default"
	if configPath != "" && pathExists(configPath) && cfg.ToolMode != "" {
		toolModeSrc = "config: " + configPath
	}
	if toolMode == "" {
		toolMode = "all"
	}
	fields["tool_mode"] = ConfigField{Value: toolMode, Source: toolModeSrc}

	// MCP Servers Count
	fields["mcp_servers_count"] = ConfigField{
		Value:  len(cfg.MCPServers),
		Source: "config",
	}

	explanation := EffectiveConfigExplanation{
		WorkspaceRoot: workdir,
		TrustStatus:   trustState,
		Fields:        fields,
	}

	if *jsonOutput {
		enc := json.NewEncoder(stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(explanation)
	}

	var b strings.Builder
	b.WriteString("=== Ion Effective Configuration ===\n")
	b.WriteString(fmt.Sprintf("Workspace:    %s\n", workdir))
	b.WriteString(fmt.Sprintf("Trust Status: %s\n\n", trustState))
	b.WriteString("Resolved Settings:\n")
	for name, f := range fields {
		b.WriteString(fmt.Sprintf("  %-20s %-30v [source: %s]\n", name+":", f.Value, f.Source))
	}
	if configPath != "" {
		b.WriteString(fmt.Sprintf("\nConfig File:  %s (exists: %v)\n", configPath, pathExists(configPath)))
	}
	projectRules := filepath.Join(workdir, "AGENTS.md")
	b.WriteString(fmt.Sprintf("Project Rules: %s (exists: %v)\n", projectRules, pathExists(projectRules)))

	_, err = fmt.Fprint(stdout, b.String())
	return err
}
