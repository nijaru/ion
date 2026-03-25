package session

type Role string

const (
	User      Role = "user"
	Assistant Role = "assistant"
	System    Role = "system"
	Tool      Role = "tool"
	Agent     Role = "agent"
)

type Entry struct {
	Role      Role
	Title     string
	Content   string
	Reasoning string
	IsError   bool
}
