// Package workspace provides validated rooted filesystem access for
// workspace-aware agents and hosts.
//
// Validator canonicalizes the workspace root and rejects malformed,
// absolute, traversal, over-deep, or symlink-escaping paths before Root
// delegates to os.Root for capability-based containment.
//
// WorkspaceFS is the first-class rooted filesystem capability. Root currently
// implements it, and WorkspaceFS-backed search indexing plus ESCALATE.md
// parsing build on the same rooted substrate.
package workspace
