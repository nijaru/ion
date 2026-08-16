package app

import (
	"fmt"
	"net/url"
	"strings"

	"github.com/nijaru/ion/config"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/llm"
)

var (
	loadStableConfig = config.LoadStable
	loadConfigFile   = config.Load
	saveConfigFile   = config.Save
	saveProviderKey  = config.SaveAPIKey
)

func (m Model) openAPIKeyPrompt(
	cfg *config.Config,
	provider string,
	preset Preset,
) (Model, tea.Cmd) {
	def, ok := llm.LookupConfig(cfg, provider)
	if !ok {
		return m, cmdError(fmt.Sprintf("unsupported provider %q", strings.TrimSpace(provider)))
	}
	if !providerSupportsAPIKeyPrompt(def) {
		return m, cmdError(fmt.Sprintf("%s does not use API keys", def.DisplayName))
	}
	if cfg == nil {
		cfg = &config.Config{}
	}
	cfgCopy := *cfg
	cfgCopy.Provider = def.ID
	m.pickerReducer().openSetup(setupPromptState{
		kind:         SetupPromptAPIKey,
		provider:     def.ID,
		providerName: def.DisplayName,
		preset:       preset,
		cfg:          cfgCopy,
	})
	return m, nil
}

func providerSupportsAPIKeyPrompt(def llm.Definition) bool {
	if def.ID == "openai-codex" {
		return false
	}
	switch def.AuthKind {
	case llm.AuthAPIKey, llm.AuthToken, llm.AuthOptional:
		return true
	default:
		return false
	}
}

func (m Model) openEndpointPrompt(cfg *config.Config, preset Preset) (Model, tea.Cmd) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	cfgCopy := *cfg
	cfgCopy.Provider = llm.OpenAICompatibleID
	m.pickerReducer().openSetup(setupPromptState{
		kind:         SetupPromptEndpoint,
		provider:     llm.OpenAICompatibleID,
		providerName: llm.DisplayName(llm.OpenAICompatibleID),
		value:        strings.TrimSpace(cfgCopy.Endpoint),
		preset:       preset,
		cfg:          cfgCopy,
	})
	return m, nil
}

func (m Model) openModelIDPrompt(cfg *config.Config, preset Preset) (Model, tea.Cmd) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	cfgCopy := *cfg
	provider := strings.TrimSpace(cfgCopy.Provider)
	if provider == "" {
		return m, cmdError("choose a provider before entering a model ID")
	}
	providerName := llm.DisplayName(provider)
	m.pickerReducer().openSetup(setupPromptState{
		kind:         SetupPromptModelID,
		provider:     provider,
		providerName: providerName,
		preset:       preset,
		cfg:          cfgCopy,
	})
	return m, nil
}

func (m Model) handleSetupPromptKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Setup == nil {
		return m, nil
	}
	if m.Picker.Setup.saving {
		return m, nil
	}
	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		m.pickerReducer().closeSetup()
		return m, nil
	case "backspace":
		m.pickerReducer().backspaceSetupValue()
		return m, nil
	case "enter":
		return m.commitSetupPrompt()
	default:
		if text, ok := keyTextInput(msg); ok {
			m.pickerReducer().appendSetupValue(text)
		}
		return m, nil
	}
}

func (m Model) handleSetupPromptPaste(msg tea.PasteMsg) (Model, tea.Cmd) {
	if m.Picker.Setup == nil {
		return m, nil
	}
	if m.Picker.Setup.saving {
		return m, nil
	}
	content := inlinePasteText(msg.Content)
	if content == "" {
		return m, nil
	}
	m.pickerReducer().appendSetupValue(content)
	return m, nil
}

func (m Model) commitSetupPrompt() (Model, tea.Cmd) {
	prompt := m.Picker.Setup
	if prompt == nil {
		return m, nil
	}
	if m.localCommandBusy() {
		message := m.localCommandBusyMessage("saving provider setup")
		m.pickerReducer().setSetupError(message)
		return m, cmdError(message)
	}
	switch prompt.kind {
	case SetupPromptModelID:
		model := strings.TrimSpace(prompt.value)
		if model == "" {
			m.pickerReducer().setSetupError("model ID cannot be empty")
			return m, nil
		}
		if strings.TrimSpace(prompt.cfg.Provider) == "" {
			m.pickerReducer().setSetupError("choose a provider before entering a model ID")
			return m, nil
		}
		currentCfg, err := m.runtimeConfigForPreset(&prompt.cfg, prompt.preset)
		if err != nil {
			m.pickerReducer().setSetupError(fmt.Sprintf("failed to resolve provider: %v", err))
			return m, nil
		}
		if strings.EqualFold(strings.TrimSpace(currentCfg.Model), model) {
			m.pickerReducer().closeSetup()
			return m, nil
		}
		transition, _, err := m.modelSelectionTransition(&prompt.cfg, prompt.preset, model)
		if err != nil {
			m.pickerReducer().setSetupError(fmt.Sprintf("failed to set model: %v", err))
			return m, nil
		}
		retry := *prompt
		retry.err = ""
		retry.saving = false
		retry.request = 0
		m.pickerReducer().closeSetup()
		return m.switchRuntimeCommandWithOptions(
			transition,
			systemEntry("Model set to "+model),
			m.currentResumeLeafID(),
			false,
			runtimeSwitchOptions{retrySetup: &retry},
		)
	case SetupPromptAPIKey:
		key := strings.TrimSpace(prompt.value)
		if key == "" {
			m.pickerReducer().setSetupError("API key cannot be empty")
			return m, nil
		}
		requestID, ok := m.pickerReducer().beginSetupSave()
		if !ok {
			return m, nil
		}
		m.progressReducer().beginLocalStatus("Saving provider setup...")
		cfg := prompt.cfg
		provider := prompt.provider
		preset := prompt.preset
		return m, func() tea.Msg {
			err := saveProviderKey(provider, key)
			return setupPromptSavedMsg{
				requestID: requestID,
				cfg:       cfg,
				preset:    preset,
				err:       err,
			}
		}
	case SetupPromptEndpoint:
		endpoint, err := normalizeOpenAICompatibleEndpoint(prompt.value)
		if err != nil {
			m.pickerReducer().setSetupError(err.Error())
			return m, nil
		}
		requestID, ok := m.pickerReducer().beginSetupSave()
		if !ok {
			return m, nil
		}
		m.progressReducer().beginLocalStatus("Saving provider setup...")
		cfg := prompt.cfg
		cfg.Endpoint = endpoint
		preset := prompt.preset
		return m, func() tea.Msg {
			stable, err := loadStableConfig()
			if err != nil {
				return setupPromptSavedMsg{requestID: requestID, err: err}
			}
			stable.Endpoint = endpoint
			if err := saveConfigFile(stable); err != nil {
				return setupPromptSavedMsg{requestID: requestID, err: err}
			}
			return setupPromptSavedMsg{
				requestID: requestID,
				cfg:       cfg,
				preset:    preset,
			}
		}
	default:
		m.pickerReducer().closeSetup()
		return m, nil
	}
}

func (m Model) handleSetupPromptSaved(msg setupPromptSavedMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		if !m.pickerReducer().failSetupSave(msg.requestID, msg.err.Error()) {
			return m, nil
		}
		m.progressReducer().clearLocalBusyStatus()
		return m, nil
	}
	if !m.pickerReducer().completeSetupSave(msg.requestID) {
		return m, nil
	}
	m.progressReducer().clearLocalBusyStatus()
	cfg := msg.cfg
	return m.openModelSelectionForPreset(&cfg, msg.preset)
}

func normalizeOpenAICompatibleEndpoint(raw string) (string, error) {
	value := strings.TrimSpace(raw)
	if value == "" {
		return "", fmt.Errorf("endpoint cannot be empty")
	}
	if !strings.Contains(value, "://") {
		value = "http://" + value
	}
	parsed, err := url.Parse(value)
	if err != nil || parsed.Host == "" {
		return "", fmt.Errorf("invalid endpoint")
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return "", fmt.Errorf("endpoint must use http or https")
	}
	if parsed.Path == "" || parsed.Path == "/" {
		parsed.Path = "/v1"
	}
	parsed.RawQuery = ""
	parsed.Fragment = ""
	return strings.TrimRight(parsed.String(), "/"), nil
}
