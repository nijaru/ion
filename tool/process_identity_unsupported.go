//go:build !darwin && !linux

package tool

import (
	"context"
	"fmt"
)

type unsupportedProcessPlatform struct{}

func hostProcessPlatform() processPlatform { return unsupportedProcessPlatform{} }

func (unsupportedProcessPlatform) name() string { return "unsupported" }

func (unsupportedProcessPlatform) capture(int) (ProcessIdentity, error) {
	return ProcessIdentity{}, ErrProcessIdentityUnsupported
}

func (unsupportedProcessPlatform) inspect(int) (ProcessIdentity, error) {
	return ProcessIdentity{}, ErrProcessIdentityUnsupported
}

func (unsupportedProcessPlatform) terminateGroup(context.Context, ProcessIdentity) error {
	return fmt.Errorf("%w: process-group termination", ErrProcessIdentityUnsupported)
}

func (unsupportedProcessPlatform) groupExists(int) (bool, error) {
	return false, ErrProcessIdentityUnsupported
}
