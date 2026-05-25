package tools

import "github.com/nijaru/ion/internal/apperrors"

func toolContextErr(toolName string, err error) error {
	return apperrors.WrapContext(toolName+" tool", err)
}
