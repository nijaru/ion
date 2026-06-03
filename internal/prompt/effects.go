package prompt

// SideEffects describes whether a mutator changes durable session or external
// state beyond request shaping.
type SideEffects struct {
	Session  bool
	External bool
}

// HasSideEffects reports whether durable session or external state is mutated.
func (e SideEffects) HasSideEffects() bool {
	return e.Session || e.External
}

func (e SideEffects) merge(other SideEffects) SideEffects {
	return SideEffects{
		Session:  e.Session || other.Session,
		External: e.External || other.External,
	}
}

// EffectDescriber is implemented by mutators that expose their side-effect
// characteristics.
type EffectDescriber interface {
	Effects() SideEffects
}

func requestProcessorEffects(r RequestProcessor) SideEffects {
	if r == nil {
		return SideEffects{}
	}
	if d, ok := r.(EffectDescriber); ok {
		return d.Effects()
	}
	return SideEffects{}
}

func mutatorEffects(m ContextMutator) SideEffects {
	if m == nil {
		return SideEffects{}
	}
	if d, ok := m.(EffectDescriber); ok {
		return d.Effects()
	}
	return SideEffects{Session: true}
}
