package workspace

import (
	"bytes"
	"io"
	"io/fs"
	"os"
	"path"
	"time"
)

type overlayFSView struct {
	overlay *OverlayFS
}

func (v overlayFSView) Open(name string) (fs.File, error) {
	if v.overlay == nil {
		return nil, os.ErrInvalid
	}
	name = path.Clean(name)
	info, err := v.overlay.Stat(name)
	if err == nil && !info.IsDir() {
		data, err := v.overlay.ReadFile(name)
		if err != nil {
			return nil, err
		}
		return &overlayReadFile{
			name:   name,
			reader: bytes.NewReader(data),
			info:   info,
		}, nil
	}

	entries, dirErr := v.overlay.ReadDir(name)
	if dirErr != nil {
		if err != nil {
			return nil, err
		}
		return nil, dirErr
	}
	if info == nil {
		info = overlayDirectoryInfo(name)
	}
	return &overlayReadDirFile{
		name:    name,
		info:    info,
		entries: entries,
	}, nil
}

type overlayReadFile struct {
	name   string
	reader *bytes.Reader
	info   fs.FileInfo
}

func (f *overlayReadFile) Stat() (fs.FileInfo, error) { return f.info, nil }
func (f *overlayReadFile) Close() error               { return nil }
func (f *overlayReadFile) Read(p []byte) (int, error) { return f.reader.Read(p) }

type overlayReadDirFile struct {
	name    string
	info    fs.FileInfo
	entries []fs.DirEntry
	offset  int
}

func (f *overlayReadDirFile) Stat() (fs.FileInfo, error) { return f.info, nil }
func (f *overlayReadDirFile) Close() error               { return nil }

func (f *overlayReadDirFile) Read([]byte) (int, error) {
	return 0, &fs.PathError{Op: "read", Path: f.name, Err: os.ErrInvalid}
}

func (f *overlayReadDirFile) ReadDir(n int) ([]fs.DirEntry, error) {
	if f.offset >= len(f.entries) {
		if n <= 0 {
			return nil, nil
		}
		return nil, io.EOF
	}
	if n <= 0 || f.offset+n > len(f.entries) {
		n = len(f.entries) - f.offset
	}
	entries := f.entries[f.offset : f.offset+n]
	f.offset += n
	return entries, nil
}

func overlayDirectoryInfo(name string) fs.FileInfo {
	return &overlayFileInfo{
		f: &overlayFile{
			name:    path.Base(name),
			perm:    os.ModeDir | 0o555,
			modTime: time.Time{},
			isDir:   true,
		},
	}
}
