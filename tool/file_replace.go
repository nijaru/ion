package tool

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
)

// replaceFile commits a prepared mutation through one temporary-file and
// rename protocol for every file-mutating tool.
func replaceFile(ctx context.Context, operation, path string, data []byte, mode os.FileMode) error {
	tmpPath, err := writeTempFile(path, data, mode)
	if err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		_ = os.Remove(tmpPath)
		return toolContextErr(operation, err)
	}
	if err := os.Rename(tmpPath, path); err != nil {
		_ = os.Remove(tmpPath)
		return err
	}
	return nil
}

func writeTempFile(path string, data []byte, mode os.FileMode) (string, error) {
	dir := filepath.Dir(path)
	base := filepath.Base(path)
	for attempt := 0; attempt < 16; attempt++ {
		suffix, err := randomHexSuffix()
		if err != nil {
			return "", err
		}
		name := filepath.Join(dir, "."+base+"."+suffix+".tmp")
		file, err := os.OpenFile(name, os.O_WRONLY|os.O_CREATE|os.O_EXCL, mode)
		if errors.Is(err, os.ErrExist) {
			continue
		}
		if err != nil {
			return "", err
		}
		if _, err := file.Write(data); err != nil {
			_ = file.Close()
			_ = os.Remove(name)
			return "", err
		}
		if err := file.Close(); err != nil {
			_ = os.Remove(name)
			return "", err
		}
		return name, nil
	}
	return "", fmt.Errorf("could not create temporary file for %s", path)
}

func randomHexSuffix() (string, error) {
	var buf [8]byte
	if _, err := io.ReadFull(rand.Reader, buf[:]); err != nil {
		return "", err
	}
	return hex.EncodeToString(buf[:]), nil
}
