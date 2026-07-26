package agent

import "errors"

// HookHandler is a function registered for a hook type. It receives a payload
// and returns a patch (nil if no change) or an error.
// Reference: Pi agent-harness.js hooks (line 944-960).
type HookHandler func(payload any) (patch any, err error)

// Hook type constants matching Pi's hook names.
const (
	HookBeforeProviderRequest = "before_provider_request"
	HookBeforeProviderPayload = "before_provider_payload"
	HookAfterProviderResponse = "after_provider_response"
	HookBeforeAgentStart      = "before_agent_start"
	HookBeforeToolCall        = "before_tool_call"
	HookToolResult            = "tool_result"
)

type hookRegistration struct {
	id      uint64
	handler HookHandler
}

// On registers a handler for a hook type. Returns an idempotent unsubscribe
// function. Registration identity is stable across slice growth, so later
// registrations cannot invalidate an existing unsubscribe closure.
// Reference: Pi agent-harness.js on (line 962).
func (h *Controller) On(hookType string, handler HookHandler) func() {
	h.mu.Lock()
	h.nextHookID++
	id := h.nextHookID
	h.hooks[hookType] = append(h.hooks[hookType], hookRegistration{id: id, handler: handler})
	h.mu.Unlock()

	return func() {
		h.mu.Lock()
		defer h.mu.Unlock()
		registrations := h.hooks[hookType]
		for i, registration := range registrations {
			if registration.id != id {
				continue
			}
			copy(registrations[i:], registrations[i+1:])
			registrations[len(registrations)-1] = hookRegistration{}
			h.hooks[hookType] = registrations[:len(registrations)-1]
			return
		}
	}
}

// emitHook fans out a payload to all handlers registered for hookType.
// Returns collected patches. Uses snapshot-and-release to avoid reentry deadlock.
func (h *Controller) emitHook(hookType string, payload any) (patches []any, err error) {
	h.mu.Lock()
	snapshot := make([]HookHandler, 0, len(h.hooks[hookType]))
	for _, registration := range h.hooks[hookType] {
		if registration.handler != nil {
			snapshot = append(snapshot, registration.handler)
		}
	}
	h.mu.Unlock()

	var hookErrors []error
	for _, fn := range snapshot {
		patch, fnErr := fn(payload)
		if fnErr != nil {
			hookErrors = append(hookErrors, fnErr)
			continue
		}
		if patch != nil {
			patches = append(patches, patch)
		}
	}
	return patches, errors.Join(hookErrors...)
}
