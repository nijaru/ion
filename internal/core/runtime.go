package core

import (
	"github.com/nijaru/ion/config"
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/session"
)

type Preset string

const (
	PresetPrimary Preset = "primary"
	PresetFast    Preset = "fast"
)

func (p Preset) String() string {
	switch p {
	case PresetFast:
		return string(PresetFast)
	default:
		return string(PresetPrimary)
	}
}

func PresetFromString(value string) Preset {
	if config.NormalizeActivePreset(value) == string(PresetFast) {
		return PresetFast
	}
	return PresetPrimary
}

type Switcher func(context.Context, *config.Config, string) (Backend, session.AgentSession, session.SessionHandle, error)

type Handles struct {
	Backend Backend
	Session session.AgentSession
	Storage session.SessionHandle
}

type Snapshot struct {
	AppConfig     config.Config
	BackendConfig config.Config
	Preset        Preset
	Provider      string
	Model         string
	Reasoning     string
	SessionID     string
	Materialized  bool
	Status        string
}

func NewSnapshot(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset Preset,
	status string,
) Snapshot {
	if appCfg == nil {
		appCfg = backendCfg
	}

	var appCopy config.Config
	if appCfg != nil {
		appCopy = *appCfg
	}

	backendCopy := appCopy
	if backendCfg != nil {
		backendCopy = *backendCfg
	}

	if preset == "" {
		preset = PresetPrimary
	}

	return Snapshot{
		AppConfig:     appCopy,
		BackendConfig: backendCopy,
		Preset:        preset,
		Provider:      strings.TrimSpace(backendCopy.Provider),
		Model:         strings.TrimSpace(backendCopy.Model),
		Reasoning:     config.NormalizeReasoningEffort(backendCopy.ReasoningEffort),
		Status:        status,
	}
}

func (s Snapshot) WithHandles(handles Handles) Snapshot {
	if s.Provider == "" && handles.Backend != nil {
		s.Provider = strings.TrimSpace(handles.Backend.Provider())
	}
	if s.Model == "" && handles.Backend != nil {
		s.Model = strings.TrimSpace(handles.Backend.Model())
	}
	if s.Reasoning == "" {
		s.Reasoning = config.DefaultReasoningEffort
	}
	s.SessionID, s.Materialized = SessionState(handles)
	return s
}

func (s Snapshot) MaterializedSessionID() string {
	if !s.Materialized {
		return ""
	}
	return strings.TrimSpace(s.SessionID)
}

type Transition struct {
	Snapshot             Snapshot
	PersistState         bool
	PersistReasoning     bool
	PersistActivePreset  bool
	PersistReasoningSlot Preset
	PersistReasoningText string
}

func NewTransition(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset Preset,
	status string,
) Transition {
	return Transition{
		Snapshot: NewSnapshot(appCfg, backendCfg, preset, status),
	}
}

func (t Transition) WithStatus(status string) Transition {
	t.Snapshot.Status = status
	return t
}

func (t Transition) WithHandles(handles Handles) Transition {
	t.Snapshot = t.Snapshot.WithHandles(handles)
	return t
}

func (t Transition) WithStatePersistence() Transition {
	t.PersistState = true
	return t
}

func (t Transition) WithReasoningPersistence(
	preset Preset,
	effort string,
) Transition {
	t.PersistReasoning = true
	t.PersistReasoningSlot = preset
	t.PersistReasoningText = effort
	return t
}

func (t Transition) WithActivePresetPersistence() Transition {
	t.PersistActivePreset = true
	return t
}

func (t Transition) NeedsPersistence() bool {
	return t.PersistState || t.PersistReasoning || t.PersistActivePreset
}

type SaveStateFunc func(config.RuntimeStateUpdate) error

func (t Transition) Persist(save SaveStateFunc) error {
	if !t.NeedsPersistence() {
		return nil
	}
	if save == nil {
		return fmt.Errorf("save state: missing runtime state saver")
	}
	update := config.RuntimeStateUpdate{
		Config:              &t.Snapshot.AppConfig,
		PersistConfig:       t.PersistState,
		ActivePreset:        t.Snapshot.Preset.String(),
		PersistActivePreset: t.PersistActivePreset,
		ReasoningPreset:     t.PersistReasoningSlot.String(),
		ReasoningEffort:     t.PersistReasoningText,
		PersistReasoning:    t.PersistReasoning,
	}
	if err := save(update); err != nil {
		return fmt.Errorf("save state: %w", err)
	}
	return nil
}

type Accepted struct {
	Transition Transition
	Handles    Handles
}

func NewAccepted(transition Transition, handles Handles) Accepted {
	return Accepted{
		Transition: transition.WithHandles(handles),
		Handles:    handles,
	}
}

type SwitchInput struct {
	Switcher        Switcher
	Transition      Transition
	Current         Handles
	TargetSessionID string
	PreserveSession bool
	SaveState       SaveStateFunc
}

type SwitchResult struct {
	Runtime  Accepted
	Previous Handles
}

func Switch(ctx context.Context, input SwitchInput) (SwitchResult, error) {
	result, err := openRuntime(ctx, input)
	if err != nil {
		return SwitchResult{}, err
	}
	if err := input.Transition.Persist(input.SaveState); err != nil {
		CloseHandles(result.Runtime.Handles)
		return SwitchResult{}, err
	}
	return result, nil
}

type ResumeInput struct {
	Switcher   Switcher
	Transition Transition
	Current    Handles
	SessionID  string
	SaveState  SaveStateFunc
}

type ResumeResult struct {
	SwitchResult
	Entries []session.Entry
}

func Resume(ctx context.Context, input ResumeInput) (ResumeResult, error) {
	result, err := openRuntime(ctx, SwitchInput{
		Switcher:        input.Switcher,
		Transition:      input.Transition,
		Current:         input.Current,
		TargetSessionID: input.SessionID,
		SaveState:       input.SaveState,
	})
	if err != nil {
		return ResumeResult{}, err
	}
	handles := result.Runtime.Handles
	var entries []session.Entry
	if handles.Storage != nil {
		entries, err = handles.Storage.Entries(ctx)
		if err != nil {
			CloseHandles(handles)
			return ResumeResult{}, fmt.Errorf("load session transcript: %w", err)
		}
	}
	if err := input.Transition.Persist(input.SaveState); err != nil {
		CloseHandles(handles)
		return ResumeResult{}, err
	}
	return ResumeResult{
		SwitchResult: result,
		Entries:      entries,
	}, nil
}

func openRuntime(ctx context.Context, input SwitchInput) (SwitchResult, error) {
	if input.Switcher == nil {
		return SwitchResult{}, fmt.Errorf("runtime switcher unavailable")
	}
	targetSessionID := input.TargetSessionID
	if input.PreserveSession && targetSessionID == "" && input.Current.Session != nil {
		targetSessionID = input.Current.Session.ID()
	}
	cfgCopy := input.Transition.Snapshot.BackendConfig
	backend, sess, storageSess, err := input.Switcher(ctx, &cfgCopy, targetSessionID)
	if err != nil {
		return SwitchResult{}, err
	}
	handles := Handles{
		Backend: backend,
		Session: sess,
		Storage: storageSess,
	}
	status := ""
	if backend != nil {
		status = backend.Bootstrap().Status
	}
	return SwitchResult{
		Runtime: NewAccepted(input.Transition.WithStatus(status), handles),
		Previous: Handles{
			Session: input.Current.Session,
			Storage: input.Current.Storage,
		},
	}, nil
}

func CloseHandles(handles Handles) {
	if handles.Session != nil {
		_ = handles.Session.Close()
	}
	if handles.Storage != nil {
		_ = handles.Storage.Close()
	}
}

func SessionState(handles Handles) (string, bool) {
	if handles.Storage != nil {
		id := strings.TrimSpace(handles.Storage.ID())
		return id, session.IsMaterialized(handles.Storage)
	}
	if handles.Session == nil {
		return "", false
	}
	id := strings.TrimSpace(handles.Session.ID())
	return id, id != ""
}
