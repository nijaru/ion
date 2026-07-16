// Package mcp exposes Ion tools over the Model Context Protocol and adapts
// remote MCP tools back into Ion's tool registry.
//
// The package uses the official Go MCP SDK for transport and session handling.
// Ion still owns tool validation, registry integration, and the distinction
// between framework-level tool semantics and application-level tool policy.
//
// Use NewClient or NewStdioClient for one-off integration. Runtime startup
// should use Open, which namespaces discovered tools and owns every stdio
// subprocess until Close. Use NewServer when you want to serve an existing
// registry over an MCP transport.
package mcp
