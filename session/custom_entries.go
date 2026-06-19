package session

import "context"

// LabelChangedData records a label change on a target entry.
type LabelChangedData struct {
	TargetID string `json:"target_id"`
	Label    string `json:"label"`
}

// SessionInfoChangedData records a session name change.
type SessionInfoChangedData struct {
	Name string `json:"name"`
}

// CustomEntryData records a custom entry (for extensions).
type CustomEntryData struct {
	CustomType string `json:"custom_type"`
	Data       any    `json:"data,omitempty"`
}

// CustomMessageEntryData records a custom message entry (for extensions).
type CustomMessageEntryData struct {
	CustomType string `json:"custom_type"`
	Content    string `json:"content"`
	Display    string `json:"display,omitempty"`
	Details    string `json:"details,omitempty"`
}

// AppendLabel appends a label entry that tags targetID with the given label.
// Pass empty label to clear. Returns the event ID.
func (s *Session) AppendLabel(ctx context.Context, targetID string, label string) (string, error) {
	e := NewEvent(s.ID(), LabelChanged, LabelChangedData{
		TargetID: targetID,
		Label:    label,
	})
	if err := s.Append(ctx, e); err != nil {
		return "", err
	}
	return e.ID.String(), nil
}

// AppendCustomEntry appends a custom entry (for extensions).
// Returns the event ID.
func (s *Session) AppendCustomEntry(ctx context.Context, customType string, data any) (string, error) {
	e := NewEvent(s.ID(), CustomEntry, CustomEntryData{
		CustomType: customType,
		Data:       data,
	})
	if err := s.Append(ctx, e); err != nil {
		return "", err
	}
	return e.ID.String(), nil
}

// AppendCustomMessageEntry appends a custom message entry (for extensions).
// Returns the event ID.
func (s *Session) AppendCustomMessageEntry(ctx context.Context, customType string, content string, display string, details string) (string, error) {
	e := NewEvent(s.ID(), CustomMessageEntry, CustomMessageEntryData{
		CustomType: customType,
		Content:    content,
		Display:    display,
		Details:    details,
	})
	if err := s.Append(ctx, e); err != nil {
		return "", err
	}
	return e.ID.String(), nil
}

// AppendSessionInfo appends a session info entry (e.g., display name).
// Returns the event ID.
func (s *Session) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	e := NewEvent(s.ID(), SessionInfoChanged, SessionInfoChangedData{
		Name: name,
	})
	if err := s.Append(ctx, e); err != nil {
		return "", err
	}
	return e.ID.String(), nil
}
