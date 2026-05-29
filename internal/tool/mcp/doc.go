// Package mcp exposes Canto tools over the Model Context Protocol and adapts
// remote MCP tools back into Canto's tool registry.
//
// The package uses the official Go MCP SDK for transport and session handling.
// Canto still owns tool validation, registry integration, and the distinction
// between framework-level tool semantics and application-level tool policy.
//
// Use NewClient or NewStdioClient to discover remote tools and register them
// in a tool.Registry. Use NewServer when you want to serve an existing
// registry over an MCP transport.
package mcp
