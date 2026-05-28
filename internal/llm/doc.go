// Package llm defines Canto's provider-agnostic model interface.
//
// Request, Response, Message, Call, and Spec are the normalized types shared by
// the rest of the framework. Provider is the core backend contract for text
// generation, streaming, token counting, pricing, and capability reporting.
//
// Registry, SmartResolver, and FailoverProvider help compose multiple
// providers, while concrete implementations live under llm/providers.
package llm
