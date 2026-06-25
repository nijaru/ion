package app

import (
	"runtime"
	"strings"
)

// formatKeyPart formats a single key part for display.
// On macOS, "alt" is displayed as "option".
func formatKeyPart(part string, capitalize bool) string {
	display := part
	if runtime.GOOS == "darwin" && strings.ToLower(part) == "alt" {
		display = "option"
	}
	if capitalize && len(display) > 0 {
		display = strings.ToUpper(display[:1]) + display[1:]
	}
	return display
}

// formatKeyText formats a key string like "ctrl+l" or "alt+enter" for display.
// Handles compound keys like "ctrl+shift+l" and alternatives like "ctrl+j/shift+enter".
func formatKeyText(key string, capitalize bool) string {
	parts := strings.Split(key, "/")
	result := make([]string, len(parts))
	for i, part := range parts {
		components := strings.Split(part, "+")
		for j, comp := range components {
			components[j] = formatKeyPart(comp, capitalize)
		}
		result[i] = strings.Join(components, "+")
	}
	return strings.Join(result, "/")
}
