package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func startupBannerLines(version string) []string {
	version = strings.TrimSpace(version)

	if version == "" {
		version = "v0.0.0"
	}
	return []string{"ion " + version}
}

func startupToolLine(b app.RuntimeInfo) string {
	summarizer, ok := b.(app.ToolSummarizer)
	if !ok {
		return ""
	}
	surface := summarizer.ToolSurface()
	if surface.Count == 0 {
		return ""
	}
	var parts []string
	if surface.LazyEnabled {
		parts = append(parts, "Search tools enabled")
	}
	sandbox := strings.TrimSpace(surface.Sandbox)
	if sandbox != "" && sandbox != "off" {
		parts = append(parts, "Sandbox "+sandbox)
	}
	if len(parts) == 0 {
		return ""
	}
	return strings.Join(parts, " • ")
}

func startupKeyboardLine() string {
	if strings.TrimSpace(os.Getenv("TMUX")) == "" {
		return ""
	}
	return tmuxKeyboardLine(showTmuxOption)
}

func tmuxKeyboardLine(show func(string) (string, error)) string {
	extendedKeys, err := show("extended-keys")
	if err != nil {
		return ""
	}
	switch strings.TrimSpace(extendedKeys) {
	case "on", "always":
	default:
		return "tmux extended-keys is off; Shift+Enter may submit. Use Ctrl+J for newline or enable tmux extended-keys."
	}

	extendedKeysFormat, err := show("extended-keys-format")
	if err != nil {
		return ""
	}
	if strings.TrimSpace(extendedKeysFormat) == "xterm" {
		return "tmux extended-keys-format is xterm; Shift+Enter may be unreliable. Use Ctrl+J for newline or set extended-keys-format csi-u."
	}
	return ""
}

func showTmuxOption(option string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "tmux", "show", "-gv", option).Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}

func currentBranch() string {
	out, err := exec.Command("git", "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
}

func resumeHintSessionID(model tea.Model) string {
	appModel, ok := model.(*app.Model)
	if !ok || appModel == nil {
		return ""
	}
	return appModel.ResumeSessionID()
}

func printResumeHint(w io.Writer, sessionID string) {
	sessionID = strings.TrimSpace(sessionID)
	if sessionID == "" {
		return
	}
	fmt.Fprintf(w, "\nResume this session with:\nion --resume %s\n", sessionID)
}

func printStartup(
	out io.Writer,
	startupLines []string,
	workspaceLine string,
	resumed bool,
	renderedEntries []string,
) {
	if out == nil {
		return
	}
	var lines []string
	for _, line := range startupLines {
		lines = append(lines, styleStartupLine(line))
	}
	if workspaceLine != "" {
		lines = append(lines, startupWorkspaceStyle().Render(workspaceLine))
	}
	if resumed {
		lines = append(lines, "", startupMetaStyle().Render("--- resumed ---"))
	}
	if len(renderedEntries) > 0 {
		lines = append(lines, "")
	}
	lines = append(lines, renderedEntries...)
	if len(lines) == 0 {
		return
	}
	lines = append(lines, "")
	_, _ = fmt.Fprintln(out, strings.Join(lines, "\n"))
}

func workspaceHeader(cwd, branch string) string {
	home, _ := os.UserHomeDir()
	dir := shortenHomePath(cwd, home)
	parts := []string{dir}
	if strings.TrimSpace(branch) != "" {
		parts = append(parts, branch)
	}
	return strings.Join(parts, " • ")
}

func shortenHomePath(path, home string) string {
	if home == "" || path == "" {
		return path
	}
	if path == home {
		return "~"
	}
	prefix := strings.TrimRight(home, string(os.PathSeparator)) + string(os.PathSeparator)
	if strings.HasPrefix(path, prefix) {
		return "~" + string(os.PathSeparator) + strings.TrimPrefix(path, prefix)
	}
	return path
}

func styleStartupLine(line string) string {
	trimmed := strings.TrimSpace(line)
	if trimmed == "--- resumed ---" {
		return startupMetaStyle().Render(line)
	}
	if strings.HasPrefix(trimmed, "tmux ") {
		return startupWarnStyle().Render(line)
	}
	parts := strings.Split(line, " • ")
	if len(parts) == 0 {
		return line
	}
	if len(parts) >= 1 && strings.HasPrefix(parts[0], "ion ") {
		first := strings.TrimPrefix(parts[0], "ion ")
		parts[0] = startupNameStyle().Render("ion") + " " + startupVersionStyle().Render(first)
	}
	for i := 1; i < len(parts); i++ {
		parts[i] = startupMetaStyle().Render(parts[i])
	}
	sep := startupMetaStyle().Render(" • ")
	return strings.Join(parts, sep)
}

func startupNameStyle() lipgloss.Style {
	return lipgloss.NewStyle().Foreground(lipgloss.Color("6"))
}

func startupVersionStyle() lipgloss.Style {
	return lipgloss.NewStyle()
}

func startupMetaStyle() lipgloss.Style {
	return lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
}

func startupWorkspaceStyle() lipgloss.Style {
	return startupMetaStyle()
}

func startupWarnStyle() lipgloss.Style {
	return lipgloss.NewStyle().Foreground(lipgloss.Color("3"))
}

var (
	errNoProviderConfigured = errors.New(
		"no provider configured; use /provider. Set ION_PROVIDER or --provider for scripts",
	)
	errNoModelConfigured = errors.New(
		"no model configured; use /model. Set ION_MODEL or --model for scripts",
	)
)

func resolveStartupConfig(cfg *config.Config) error {
	cfg.Provider = llm.ResolveID(cfg.Provider)
	cfg.Model = strings.TrimSpace(cfg.Model)
	cfg.Endpoint = strings.TrimSpace(cfg.Endpoint)
	cfg.AuthEnvVar = strings.TrimSpace(cfg.AuthEnvVar)

	if cfg.Provider == "" {
		return errNoProviderConfigured
	}
	def, ok := llm.Lookup(cfg.Provider)
	if !ok {
		return fmt.Errorf("unsupported provider %q", cfg.Provider)
	}
	if llm.RequiresEndpoint(cfg) && llm.ResolvedEndpoint(cfg) == "" {
		return fmt.Errorf("%s requires endpoint configuration", def.DisplayName)
	}

	if cfg.Model == "" {
		return errNoModelConfigured
	}

	return nil
}

func validateAPIKeyOverride(apiKey, model string) error {
	if strings.TrimSpace(apiKey) != "" && strings.TrimSpace(model) == "" {
		return errors.New("--api-key requires --model or an effective configured model")
	}
	return nil
}

func applyCLIConfigOverrides(
	cfg *config.Config,
	providerOverride, modelOverride, thinkingOverride string,
) {
	if cfg == nil {
		return
	}
	if strings.TrimSpace(providerOverride) != "" {
		provider := llm.ResolveID(providerOverride)
		if provider != llm.ResolveID(cfg.Provider) {
			if strings.TrimSpace(modelOverride) == "" {
				cfg.Model = ""
			}
			clearProviderScopedPresets(cfg)
		}
		cfg.Provider = provider
	}
	if model := strings.TrimSpace(modelOverride); model != "" {
		if cfg.Provider == "" {
			if provider, rest, ok := strings.Cut(model, "/"); ok {
				resolved := llm.ResolveID(provider)
				if _, exists := llm.Lookup(resolved); exists {
					cfg.Provider = resolved
					cfg.Model = strings.TrimSpace(rest)
					model = ""
				}
			}
		}
		if model != "" {
			cfg.Model = model
		}
	}
	if strings.TrimSpace(thinkingOverride) != "" {
		cfg.ReasoningEffort = thinkingOverride
	}
}

func applyCLITrustModeOverride(cfg *config.Config, override string) {
	if cfg == nil || strings.TrimSpace(override) == "" {
		return
	}
	cfg.TrustMode = config.NormalizeTrustMode(override)
}

func loadEffectiveConfigForReload(
	load func() (*config.Config, error),
	providerOverride string,
	modelOverride string,
	thinkingOverride string,
	trustModeOverride string,
	apiKeyOverride string,
) (*config.Config, error) {
	if load == nil {
		load = config.Load
	}
	cfg, err := load()
	if err != nil {
		return nil, err
	}
	if cfg == nil {
		return nil, errors.New("config loader returned nil configuration")
	}
	applyCLIConfigOverrides(cfg, providerOverride, modelOverride, thinkingOverride)
	applyCLITrustModeOverride(cfg, trustModeOverride)
	cfg.APIKeyOverride = apiKeyOverride
	cfg.APIKeyOverrideProvider = llm.ResolveID(cfg.Provider)
	return cfg, nil
}

func clearProviderScopedPresets(cfg *config.Config) {
	cfg.FastModel = ""
	cfg.FastReasoningEffort = ""
	cfg.SummaryModel = ""
	cfg.SummaryReasoningEffort = ""
}

func startupRuntimeConfig(
	ctx context.Context,
	cfg *config.Config,
	sessionID string,
	explicitRuntimeOverride bool,
) (*config.Config, string, error) {
	preset := "primary"
	if !explicitRuntimeOverride && strings.TrimSpace(sessionID) == "" {
		if state, err := config.LoadState(); err == nil && state.ActivePreset != nil {
			preset = config.NormalizeActivePreset(*state.ActivePreset)
		}
	}
	if preset == "" {
		preset = "primary"
	}

	resolved, err := llm.ResolveRuntimeConfig(ctx, cfg, llm.Preset(preset))
	if err == nil {
		return resolved, preset, nil
	}
	if preset != "fast" {
		return nil, preset, err
	}

	resolved, err = llm.ResolveRuntimeConfig(ctx, cfg, llm.PresetPrimary)
	if err != nil {
		return nil, "primary", err
	}
	return resolved, "primary", nil
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
}

func applySessionConfigFromMetadata(
	ctx context.Context,
	store session.Store,
	cwd string,
	sessionID string,
	cfg *config.Config,
) error {
	if store == nil || cfg == nil || strings.TrimSpace(sessionID) == "" {
		return nil
	}
	if _, err := store.GetEntry(ctx, sessionID); err != nil {
		return fmt.Errorf("failed to inspect session %s metadata: %w", sessionID, err)
	}
	catalog, ok := store.(sessionCatalogReader)
	if !ok {
		return fmt.Errorf("session store does not support session catalog")
	}
	sessions, err := catalog.ListSessions(ctx, cwd)
	if err != nil {
		return fmt.Errorf("failed to list sessions: %w", err)
	}
	var storedModel string
	for _, info := range sessions {
		if info.ID() == sessionID {
			storedModel = info.Model
			break
		}
	}
	provider, model := splitSessionModelName(storedModel)
	if provider == "" {
		return nil
	}
	if llm.ResolveID(cfg.Provider) != llm.ResolveID(provider) {
		clearProviderScopedPresets(cfg)
	}
	cfg.Provider = provider
	cfg.Model = model
	return nil
}

func backendForProvider(provider string) (app.RuntimeInfo, error) {
	provider = llm.ResolveID(provider)
	if provider == "" {
		return nil, fmt.Errorf("no provider configured")
	}

	def, ok := llm.Lookup(provider)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", provider)
	}
	return &configBackend{def: &def}, nil
}

// configBackend is a minimal app.RuntimeInfo for native providers.
// It holds config and store, but does not own a Session (the harness does).
type configBackend struct {
	def     *llm.Definition
	cfg     *config.Config
	store   session.Store
	surface app.ToolSurface
}

func (b *configBackend) Name() string { return "ion" }
func (b *configBackend) Provider() string {
	if b.def != nil {
		return b.def.ID
	}
	return ""
}
func (b *configBackend) Model() string {
	if b.cfg != nil {
		return b.cfg.Model
	}
	return ""
}
func (b *configBackend) ContextLimit() int {
	if b.cfg != nil {
		return b.cfg.ContextLimit
	}
	return 0
}
func (b *configBackend) Bootstrap() app.Bootstrap {
	entries, _ := b.store.Entries(context.Background())
	return app.Bootstrap{Entries: entries,
		Status: fmt.Sprintf("%s/%s", b.Provider(), b.Model())}
}
func (b *configBackend) SetStore(s session.Store)     { b.store = s }
func (b *configBackend) SetConfig(cfg *config.Config) { b.cfg = cfg }
func (b *configBackend) ToolSurface() app.ToolSurface {
	surface := b.surface
	surface.Names = append([]string(nil), surface.Names...)
	surface.ActiveNames = append([]string(nil), surface.ActiveNames...)
	return surface
}

func splitSessionModelName(value string) (string, string) {
	value = strings.TrimSpace(value)
	if value == "" {
		return "", ""
	}
	provider, model, ok := strings.Cut(value, "/")
	if !ok {
		return strings.TrimSpace(value), ""
	}
	return strings.TrimSpace(provider), strings.TrimSpace(model)
}

func sessionModelName(provider, model string) string {
	provider = strings.TrimSpace(provider)
	model = strings.TrimSpace(model)

	switch {
	case provider == "":
		return model
	case model == "":
		return provider
	default:
		return provider + "/" + model
	}
}
