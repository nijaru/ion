package app

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// KeybindingAction represents a named keybinding action.
type KeybindingAction string

const (
	// Model cycling
	ActionCycleModelForward  KeybindingAction = "model.cycleForward"
	ActionCycleModelBackward KeybindingAction = "model.cycleBackward"

	// Thinking
	ActionCycleThinking KeybindingAction = "thinking.cycle"

	// Editor
	ActionExternalEditor KeybindingAction = "editor.external"

	// Tools
	ActionToggleTools KeybindingAction = "tools.expand"

	// Session
	ActionToggleNamedFilter KeybindingAction = "session.toggleNamedFilter"

	// Tree/Fork
	ActionTreeFork KeybindingAction = "tree.fork"

	// Message
	ActionQueueFollowUp KeybindingAction = "message.followUp"

	// Clipboard
	ActionPasteImage KeybindingAction = "clipboard.pasteImage"

	// History / Undo
	ActionSearchHistory KeybindingAction = "history.search"
	ActionUndo          KeybindingAction = "session.undo"
)

// DefaultKeybindings maps actions to their default key combos.
var DefaultKeybindings = map[KeybindingAction]string{
	ActionCycleModelForward:  "ctrl+l",
	ActionCycleModelBackward: "ctrl+shift+l",
	ActionCycleThinking:      "shift+tab",
	ActionExternalEditor:     "ctrl+g",
	ActionToggleTools:        "ctrl+o",
	ActionToggleNamedFilter:  "ctrl+n",
	ActionQueueFollowUp:      "alt+enter",
	ActionPasteImage:         "ctrl+v",
	ActionTreeFork:           "esc esc",
	ActionSearchHistory:      "ctrl+r",
	ActionUndo:               "ctrl+-",
}

// KeybindingsManager manages keybindings with user overrides.
type KeybindingsManager struct {
	defaults  map[KeybindingAction]string
	overrides map[KeybindingAction]string
	resolved  map[string]KeybindingAction // key -> action
}

// NewKeybindingsManager creates a new keybindings manager.
func NewKeybindingsManager() *KeybindingsManager {
	km := &KeybindingsManager{
		defaults:  DefaultKeybindings,
		overrides: make(map[KeybindingAction]string),
		resolved:  make(map[string]KeybindingAction),
	}
	km.resolve()
	return km
}

// LoadKeybindings loads user keybindings from ~/.ion/keybindings.json.
func LoadKeybindings() (*KeybindingsManager, error) {
	km := NewKeybindingsManager()

	path, err := KeybindingPath()
	if err != nil {
		return km, nil // Return defaults on error
	}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return km, nil // Return defaults if file doesn't exist
		}
		return km, fmt.Errorf("read keybindings: %w", err)
	}

	var overrides map[string]string
	if err := json.Unmarshal(data, &overrides); err != nil {
		return km, fmt.Errorf("parse keybindings: %w", err)
	}

	for action, key := range overrides {
		km.overrides[KeybindingAction(action)] = key
	}
	km.resolve()

	return km, nil
}

// KeybindingPath returns the path to the keybindings config file.
func KeybindingPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "keybindings.json"), nil
}

// resolve rebuilds the resolved keybindings map.
func (km *KeybindingsManager) resolve() {
	km.resolved = make(map[string]KeybindingAction)

	// Start with defaults
	for action, key := range km.defaults {
		km.resolved[key] = action
	}

	// Apply overrides
	for action, key := range km.overrides {
		// Remove old mapping for this action
		for k, a := range km.resolved {
			if a == action {
				delete(km.resolved, k)
			}
		}
		// Add new mapping
		km.resolved[key] = action
	}
}

// ActionForKey returns the action for a given key combo, or empty string if none.
func (km *KeybindingsManager) ActionForKey(key string) KeybindingAction {
	return km.resolved[normalizeKey(key)]
}

// KeyForAction returns the key combo for a given action, or empty string if none.
func (km *KeybindingsManager) KeyForAction(action KeybindingAction) string {
	if key, ok := km.overrides[action]; ok {
		return key
	}
	return km.defaults[action]
}

// normalizeKey normalizes a key combo string for comparison.
func normalizeKey(key string) string {
	return strings.ToLower(strings.TrimSpace(key))
}

// SaveKeybindings saves user keybindings to ~/.ion/keybindings.json.
func SaveKeybindings(overrides map[KeybindingAction]string) error {
	path, err := KeybindingPath()
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}

	data, err := json.MarshalIndent(overrides, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(path, data, 0o644)
}
