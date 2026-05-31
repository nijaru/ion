package session

import (
	"strings"
	"time"
)

type StatusChangeInput struct {
	AgentID   string
	Status    string
	Timestamp time.Time
	Now       time.Time
}

type StatusChangeDecision struct {
	Root             bool
	Status           string
	StatusUpdatedAt  time.Time
	PersistTimestamp time.Time
	Compacting       bool
}

func DecideStatusChange(input StatusChangeInput) StatusChangeDecision {
	if input.AgentID != "" {
		return StatusChangeDecision{}
	}
	updatedAt := input.Timestamp
	if updatedAt.IsZero() {
		updatedAt = input.Now
	}
	return StatusChangeDecision{
		Root:             true,
		Status:           input.Status,
		StatusUpdatedAt:  updatedAt,
		PersistTimestamp: input.Timestamp,
		Compacting:       IsCompactingStatus(input.Status),
	}
}

func IsCompactingStatus(status string) bool {
	return strings.Contains(strings.ToLower(strings.TrimSpace(status)), "compacting")
}
