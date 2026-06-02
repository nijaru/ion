package app

import (
	"time"

	"github.com/nijaru/ion/session"
)

func systemEntry(content string) session.Entry {
	entry, _ := session.EntrySystem(content, time.Time{})
	return entry
}
