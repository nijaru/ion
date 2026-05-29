package safety

import (
	"context"
	"fmt"
)

// SecretInjector resolves named secrets into environment entries at execution
// time. Callers ask for names; the injector decides where the values come from.
type SecretInjector interface {
	Inject(ctx context.Context, names []string) ([]string, error)
}

// StaticSecretInjector is a simple in-memory secret source for tests and
// embedding applications.
type StaticSecretInjector map[string]string

// Inject resolves requested names into NAME=value environment entries.
func (s StaticSecretInjector) Inject(ctx context.Context, names []string) ([]string, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if len(names) == 0 {
		return nil, nil
	}

	env := make([]string, 0, len(names))
	for _, name := range names {
		if name == "" {
			continue
		}
		value, ok := s[name]
		if !ok {
			return nil, fmt.Errorf("secret %q is unavailable", name)
		}
		env = append(env, name+"="+value)
	}
	return env, nil
}
