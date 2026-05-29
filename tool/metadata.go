package tool

import "github.com/nijaru/ion/llm"

// ConcurrencyMode describes whether a tool can be safely batched with peers.
type ConcurrencyMode string

const (
	Unknown    ConcurrencyMode = ""
	Parallel   ConcurrencyMode = "parallel"
	Serialized ConcurrencyMode = "serialized"
)

// Example captures a concrete tool invocation example without overloading the
// LLM-facing schema.
type Example struct {
	Description string `json:"description,omitzero"`
	Arguments   string `json:"arguments,omitzero"`
}

// Metadata carries scheduling and policy hints for a tool.
type Metadata struct {
	Category    string          `json:"category,omitzero"`
	ReadOnly    bool            `json:"read_only,omitzero"`
	Concurrency ConcurrencyMode `json:"concurrency,omitzero"`
	Deferred    bool            `json:"deferred,omitzero"`
	Examples    []Example       `json:"examples,omitzero"`
}

// MetadataTool is an optional extension for tools that expose metadata.
type MetadataTool interface {
	Tool
	Metadata() Metadata
}

// MetadataFor returns metadata for a tool, if any.
func MetadataFor(t Tool) Metadata {
	if mt, ok := t.(MetadataTool); ok {
		return mt.Metadata()
	}
	return Metadata{}
}

// ToolEntry is the registry view of one tool with its schema and metadata.
type ToolEntry struct {
	Name     string
	Tool     Tool
	Spec     llm.Spec
	Metadata Metadata
}
