package tool

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
)

// replaceFileWithinRoot commits a prepared mutation through a directory
// handle. The temporary file, rename, directory sync, and postimage check all
// use the same pinned parent, so a concurrent pathname swap cannot redirect
// the operation outside the approved directory.
func replaceFileWithinRoot(ctx context.Context, operation string, root *os.Root, target string, data []byte, mode os.FileMode) error {
	if root == nil {
		return errors.New("mutation root is required")
	}
	if err := ctx.Err(); err != nil {
		return toolContextErr(operation, err)
	}
	tmpName, err := writeTempFileWithinRoot(root, target, data, mode)
	if err != nil {
		return err
	}
	removeTemp := true
	defer func() {
		if removeTemp {
			_ = root.Remove(tmpName)
		}
	}()
	if err := ctx.Err(); err != nil {
		return toolContextErr(operation, err)
	}
	if err := root.Rename(tmpName, target); err != nil {
		return err
	}
	removeTemp = false

	// Make the replacement durable before acknowledging the effect.
	dir, err := root.Open(".")
	if err != nil {
		return fmt.Errorf("open mutation directory for sync: %w", err)
	}
	syncErr := dir.Sync()
	closeErr := dir.Close()
	if syncErr != nil {
		return fmt.Errorf("sync mutation directory: %w", syncErr)
	}
	if closeErr != nil {
		return fmt.Errorf("close mutation directory: %w", closeErr)
	}

	postimage, err := root.ReadFile(target)
	if err != nil {
		return fmt.Errorf("verify %s postimage: %w", operation, err)
	}
	if !bytes.Equal(postimage, data) {
		return fmt.Errorf("verify %s postimage: content changed after replacement", operation)
	}
	return nil
}

func writeTempFileWithinRoot(root *os.Root, target string, data []byte, mode os.FileMode) (string, error) {
	for attempt := 0; attempt < 16; attempt++ {
		suffix, err := randomHexSuffix()
		if err != nil {
			return "", err
		}
		name := "." + target + "." + suffix + ".tmp"
		file, err := root.OpenFile(name, os.O_WRONLY|os.O_CREATE|os.O_EXCL, mode)
		if errors.Is(err, os.ErrExist) {
			continue
		}
		if err != nil {
			return "", err
		}
		if _, err := io.Copy(file, bytes.NewReader(data)); err != nil {
			_ = file.Close()
			_ = root.Remove(name)
			return "", err
		}
		if err := file.Sync(); err != nil {
			_ = file.Close()
			_ = root.Remove(name)
			return "", err
		}
		if err := file.Close(); err != nil {
			_ = root.Remove(name)
			return "", err
		}
		return name, nil
	}
	return "", fmt.Errorf("could not create temporary file for %s", target)
}

func randomHexSuffix() (string, error) {
	var buf [8]byte
	if _, err := io.ReadFull(rand.Reader, buf[:]); err != nil {
		return "", err
	}
	return hex.EncodeToString(buf[:]), nil
}
