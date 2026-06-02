package tool

func (b *Bash) Metadata() Metadata {
	return Metadata{
		Category:    "workspace",
		Concurrency: Serialized,
	}
}

func (r *Read) Metadata() Metadata {
	return readOnlyWorkspaceMetadata()
}

func (w *Write) Metadata() Metadata {
	return mutatingWorkspaceMetadata()
}

func (e *Edit) Metadata() Metadata {
	return mutatingWorkspaceMetadata()
}

func (l *List) Metadata() Metadata {
	return readOnlyWorkspaceMetadata()
}

func (g *Grep) Metadata() Metadata {
	return readOnlyWorkspaceMetadata()
}

func (f *Find) Metadata() Metadata {
	return readOnlyWorkspaceMetadata()
}

func readOnlyWorkspaceMetadata() Metadata {
	return Metadata{
		Category:    "workspace",
		ReadOnly:    true,
		Concurrency: Parallel,
	}
}

func mutatingWorkspaceMetadata() Metadata {
	return Metadata{
		Category:    "workspace",
		Concurrency: Serialized,
	}
}
