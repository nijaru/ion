package session

import (
	"context"
	"fmt"
)

// ModelSelection records the provider/model selected for a session branch.
type ModelSelection struct {
	ProviderID string `json:"provider_id,omitzero"`
	Model      string `json:"model"`
}

// ThinkingSelection records the host's reasoning/thinking selection for a
// session branch. Hosts map Level to provider-specific request controls.
type ThinkingSelection struct {
	Level string `json:"level"`
}

// ToolSelection records the active tool names selected for a session branch.
type ToolSelection struct {
	Names []string `json:"names"`
}

// EffectiveSettings is the model, thinking, and active-tool state recovered
// from the active branch.
type EffectiveSettings struct {
	Model         ModelSelection `json:"model,omitzero"`
	HasModel      bool           `json:"has_model,omitzero"`
	ThinkingLevel string         `json:"thinking_level"`
	ActiveTools   []string       `json:"active_tools,omitzero"`
	HasTools      bool           `json:"has_tools,omitzero"`
}

// NewModelChangedEvent records a durable model selection.
func NewModelChangedEvent(sessionID string, selection ModelSelection) Event {
	return NewEvent(sessionID, ModelChanged, selection)
}

// NewThinkingChangedEvent records a durable thinking/reasoning selection.
func NewThinkingChangedEvent(sessionID string, selection ThinkingSelection) Event {
	return NewEvent(sessionID, ThinkingChanged, selection)
}

// NewToolsChangedEvent records a durable active-tool selection.
func NewToolsChangedEvent(sessionID string, selection ToolSelection) Event {
	return NewEvent(sessionID, ToolsChanged, selection)
}

// ModelSelection decodes the payload of a model-changed event.
func (e Event) ModelSelection() (ModelSelection, bool, error) {
	return decodeEventData[ModelSelection](e, ModelChanged, "model changed")
}

// ThinkingSelection decodes the payload of a thinking-changed event.
func (e Event) ThinkingSelection() (ThinkingSelection, bool, error) {
	return decodeEventData[ThinkingSelection](e, ThinkingChanged, "thinking changed")
}

// ToolSelection decodes the payload of a tools-changed event.
func (e Event) ToolSelection() (ToolSelection, bool, error) {
	return decodeEventData[ToolSelection](e, ToolsChanged, "tools changed")
}

// AppendModelSelection appends a durable model selection to the active branch.
func (s *Session) AppendModelSelection(ctx context.Context, selection ModelSelection) error {
	if selection.Model == "" {
		return fmt.Errorf("session model selection: model is required")
	}
	return s.Append(ctx, NewModelChangedEvent(s.ID(), selection))
}

// AppendThinkingSelection appends a durable thinking selection to the active branch.
func (s *Session) AppendThinkingSelection(ctx context.Context, selection ThinkingSelection) error {
	return s.Append(ctx, NewThinkingChangedEvent(s.ID(), selection))
}

// AppendToolSelection appends a durable active-tool selection to the active branch.
func (s *Session) AppendToolSelection(ctx context.Context, selection ToolSelection) error {
	selection.Names = append([]string(nil), selection.Names...)
	return s.Append(ctx, NewToolsChangedEvent(s.ID(), selection))
}

// EffectiveSettings returns model, thinking, and active-tool state at the
// active branch tip.
func (s *Session) EffectiveSettings() (EffectiveSettings, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	events, err := s.activeEventsLocked()
	if err != nil {
		return EffectiveSettings{}, err
	}
	return effectiveSettingsFromEvents(events)
}

func effectiveSettingsFromEvents(events []Event) (EffectiveSettings, error) {
	settings := EffectiveSettings{ThinkingLevel: "off"}
	for _, event := range events {
		switch event.Type {
		case ModelChanged:
			selection, ok, err := event.ModelSelection()
			if err != nil {
				return EffectiveSettings{}, err
			}
			if ok && selection.Model != "" {
				settings.Model = selection
				settings.HasModel = true
			}
		case ThinkingChanged:
			selection, ok, err := event.ThinkingSelection()
			if err != nil {
				return EffectiveSettings{}, err
			}
			if ok {
				settings.ThinkingLevel = selection.Level
				if settings.ThinkingLevel == "" {
					settings.ThinkingLevel = "off"
				}
			}
		case ToolsChanged:
			selection, ok, err := event.ToolSelection()
			if err != nil {
				return EffectiveSettings{}, err
			}
			if ok {
				settings.ActiveTools = append([]string(nil), selection.Names...)
				settings.HasTools = true
			}
		}
	}
	return settings, nil
}
