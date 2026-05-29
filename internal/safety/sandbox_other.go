//go:build !darwin && !linux

package safety

func newPlatformSandbox() Sandbox {
	return unsupportedSandbox{reason: "no sandbox backend for this platform"}
}
