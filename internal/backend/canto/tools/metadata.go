package tools

import "github.com/nijaru/canto/tool"

func (b *Bash) Metadata() tool.Metadata {
	return tool.Metadata{
		Category:    "workspace",
		Concurrency: tool.Serialized,
	}
}

func (r *Read) Metadata() tool.Metadata {
	return readOnlyWorkspaceMetadata()
}

func (w *Write) Metadata() tool.Metadata {
	return mutatingWorkspaceMetadata()
}

func (e *Edit) Metadata() tool.Metadata {
	return mutatingWorkspaceMetadata()
}

func (l *List) Metadata() tool.Metadata {
	return readOnlyWorkspaceMetadata()
}

func (g *Grep) Metadata() tool.Metadata {
	return readOnlyWorkspaceMetadata()
}

func (f *Find) Metadata() tool.Metadata {
	return readOnlyWorkspaceMetadata()
}

func readOnlyWorkspaceMetadata() tool.Metadata {
	return tool.Metadata{
		Category:    "workspace",
		ReadOnly:    true,
		Concurrency: tool.Parallel,
	}
}

func mutatingWorkspaceMetadata() tool.Metadata {
	return tool.Metadata{
		Category:    "workspace",
		Concurrency: tool.Serialized,
	}
}
