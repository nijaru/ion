package session

import "time"

type Role string

const (
	RoleUser     Role = "user"
	RoleAgent    Role = "agent"
	RoleSystem   Role = "system"
	RoleTool     Role = "tool"
	RoleSubagent Role = "subagent"
)

type Entry struct {
	Role      Role
	Timestamp time.Time
	Title     string
	Content   string
	Reasoning string
	IsError   bool
}
