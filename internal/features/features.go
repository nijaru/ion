package features

const CoreLoopOnly = true

func Disabled(feature string) string {
	return feature + " is disabled while Ion stabilizes the P1 core agent loop"
}
