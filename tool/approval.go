package tool

// Requirement describes an approval requirement for a tool call.
type Requirement struct {
	Category  string
	Operation string
	Resource  string
	Metadata  map[string]any
}

// RequirementProvider is implemented by tools that can declare an approval
// requirement for a provider-supplied argument payload.
type RequirementProvider interface {
	ApprovalRequirement(args string) (Requirement, bool, error)
}
