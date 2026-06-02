package tool

import "github.com/nijaru/ion/apperrors"

func toolContextErr(toolName string, err error) error {
	return apperrors.WrapContext(toolName+" tool", err)
}
