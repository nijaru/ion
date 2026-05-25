package app

import (
	"time"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/transcript"
)

func systemEntry(content string) session.Entry {
	entry, _ := transcript.System(content, time.Time{})
	return entry
}
