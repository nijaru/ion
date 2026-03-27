package app

import "charm.land/lipgloss/v2"

type styles struct {
	user      lipgloss.Style
	assistant lipgloss.Style
	system    lipgloss.Style
	tool      lipgloss.Style
	agent     lipgloss.Style
	dim       lipgloss.Style
	cyan      lipgloss.Style
	warn      lipgloss.Style
	sep       lipgloss.Style
	added     lipgloss.Style
	removed   lipgloss.Style
	modeRead  lipgloss.Style
	modeWrite lipgloss.Style
}

func newStyles() styles {
	return styles{
		user:      lipgloss.NewStyle().Bold(true),
		assistant: lipgloss.NewStyle().Foreground(lipgloss.Color("6")),
		system:    lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Faint(true),
		tool:      lipgloss.NewStyle().Foreground(lipgloss.Color("10")),
		agent:     lipgloss.NewStyle().Foreground(lipgloss.Color("13")),
		dim:       lipgloss.NewStyle().Faint(true),
		cyan:      lipgloss.NewStyle().Foreground(lipgloss.Color("6")),
		warn:      lipgloss.NewStyle().Foreground(lipgloss.Color("3")),
		sep:       lipgloss.NewStyle().Foreground(lipgloss.Color("8")),
		added:     lipgloss.NewStyle().Foreground(lipgloss.Color("2")),
		removed:   lipgloss.NewStyle().Foreground(lipgloss.Color("1")),
		modeRead:  lipgloss.NewStyle().Foreground(lipgloss.Color("6")).Bold(true),
		modeWrite: lipgloss.NewStyle().Foreground(lipgloss.Color("3")).Bold(true),
	}
}
