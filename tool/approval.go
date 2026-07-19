package tool

// Requirement describes an approval requirement for a tool call.
type Requirement struct {
	Category      string
	Operation     string
	Resource      string
	Paths         []string
	Environment   []string
	NetworkIntent string
	MCPIdentity   string
	Metadata      map[string]any
	AlwaysConfirm bool
}

// RequirementProvider is implemented by tools that can declare an approval
// requirement for a provider-supplied argument payload.
type RequirementProvider interface {
	ApprovalRequirement(args string) (Requirement, bool, error)
}
