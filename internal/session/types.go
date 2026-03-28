package session

type Role string

const (
	User     Role = "user"
	Agent    Role = "agent"
	System   Role = "system"
	Tool     Role = "tool"
	Subagent Role = "subagent"
)

type Entry struct {
	Role      Role
	Title     string
	Content   string
	Reasoning string
	IsError   bool
}
