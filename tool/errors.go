package tool

import "github.com/nijaru/ion/ctxerr"

func toolContextErr(toolName string, err error) error {
	return ctxerr.WrapContext(toolName+" tool", err)
}
