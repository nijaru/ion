package harness

import (
	"context"
	"sync"
)

// Extension is a process-isolated plugin that communicates over stdio JSON-RPC.
type Extension interface {
	// Name returns the extension name.
	Name() string
	// Start starts the extension process.
	Start(ctx context.Context) error
	// Stop stops the extension process.
	Stop() error
	// Call invokes a method on the extension.
	Call(ctx context.Context, method string, params any) (any, error)
	// IsRunning returns true if the extension is running.
	IsRunning() bool
}

// ExtensionManager manages extension lifecycle.
type ExtensionManager struct {
	extensions map[string]Extension
	mu         sync.RWMutex
}

// NewExtensionManager creates a new extension manager.
func NewExtensionManager() *ExtensionManager {
	return &ExtensionManager{
		extensions: make(map[string]Extension),
	}
}

// Register registers an extension.
func (m *ExtensionManager) Register(ext Extension) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.extensions[ext.Name()] = ext
}

// Get returns an extension by name.
func (m *ExtensionManager) Get(name string) (Extension, bool) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	ext, ok := m.extensions[name]
	return ext, ok
}

// StartAll starts all registered extensions.
func (m *ExtensionManager) StartAll(ctx context.Context) error {
	m.mu.RLock()
	defer m.mu.RUnlock()
	for _, ext := range m.extensions {
		if err := ext.Start(ctx); err != nil {
			return err
		}
	}
	return nil
}

// StopAll stops all registered extensions.
func (m *ExtensionManager) StopAll() error {
	m.mu.RLock()
	defer m.mu.RUnlock()
	var firstErr error
	for _, ext := range m.extensions {
		if err := ext.Stop(); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	return firstErr
}

// Call invokes a method on the named extension.
func (m *ExtensionManager) Call(ctx context.Context, name string, method string, params any) (any, error) {
	m.mu.RLock()
	ext, ok := m.extensions[name]
	m.mu.RUnlock()
	if !ok {
		return nil, &ExtensionError{Name: name, Method: method, Message: "extension not found"}
	}
	return ext.Call(ctx, method, params)
}

// Extensions returns all registered extension names.
func (m *ExtensionManager) Extensions() []string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	result := make([]string, 0, len(m.extensions))
	for name := range m.extensions {
		result = append(result, name)
	}
	return result
}

// ExtensionError represents an extension error.
type ExtensionError struct {
	Name    string
	Method  string
	Message string
}

func (e *ExtensionError) Error() string {
	return "extension " + e.Name + "." + e.Method + ": " + e.Message
}
