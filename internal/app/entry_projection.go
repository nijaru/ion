package app

import (
	"time"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func systemEntry(content string) session.Entry {
	entry, _ := storage.EntrySystem(content, time.Time{})
	return entry
}
