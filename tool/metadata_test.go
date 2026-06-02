package tool

import (
	"testing"
)

func TestToolMetadataMarksReadOnlyToolsParallelAndMutatorsSerialized(t *testing.T) {
	cwd := t.TempDir()

	for _, tt := range []struct {
		name     string
		metadata Metadata
		readOnly bool
		mode     ConcurrencyMode
	}{
		{name: "bash", metadata: NewBash(cwd).Metadata(), mode: Serialized},
		{name: "read", metadata: (&Read{}).Metadata(), readOnly: true, mode: Parallel},
		{name: "write", metadata: (&Write{}).Metadata(), mode: Serialized},
		{name: "edit", metadata: (&Edit{}).Metadata(), mode: Serialized},
		{name: "ls", metadata: (&List{}).Metadata(), readOnly: true, mode: Parallel},
		{name: "grep", metadata: (&Grep{}).Metadata(), readOnly: true, mode: Parallel},
		{name: "find", metadata: (&Find{}).Metadata(), readOnly: true, mode: Parallel},
	} {
		t.Run(tt.name, func(t *testing.T) {
			if tt.metadata.Category != "workspace" {
				t.Fatalf("category = %q, want workspace", tt.metadata.Category)
			}
			if tt.metadata.ReadOnly != tt.readOnly {
				t.Fatalf("readOnly = %v, want %v", tt.metadata.ReadOnly, tt.readOnly)
			}
			if tt.metadata.Concurrency != tt.mode {
				t.Fatalf("concurrency = %q, want %q", tt.metadata.Concurrency, tt.mode)
			}
		})
	}
}
