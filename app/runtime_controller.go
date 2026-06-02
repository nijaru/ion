package app

import "github.com/nijaru/ion/internal/core"

// Re-export runtime types from core.
type Preset = core.Preset
type Switcher = core.Switcher
type Handles = core.Handles
type Snapshot = core.Snapshot
type Transition = core.Transition
type Accepted = core.Accepted
type SwitchInput = core.SwitchInput
type SwitchResult = core.SwitchResult
type ResumeInput = core.ResumeInput
type ResumeResult = core.ResumeResult
type SaveStateFunc = core.SaveStateFunc

// Re-export constructors and functions.
var (
	PresetFromString = core.PresetFromString
	NewSnapshot      = core.NewSnapshot
	NewTransition    = core.NewTransition
	NewAccepted      = core.NewAccepted
	Switch           = core.Switch
	Resume           = core.Resume
	CloseHandles     = core.CloseHandles
	SessionState     = core.SessionState
)

// Re-export constants (must use const, not var, for string types).
const (
	PresetPrimary = core.PresetPrimary
	PresetFast    = core.PresetFast
)
