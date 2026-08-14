package agent

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"

	"github.com/nijaru/ion/session"
)

const maxActionPreimageBytes = 64 << 20

type actionPreimage struct {
	Path          string `json:"path"`
	Exists        bool   `json:"exists"`
	Size          int64  `json:"size,omitempty"`
	Mode          uint32 `json:"mode,omitempty"`
	ModTimeUnixNS int64  `json:"mod_time_unix_ns,omitempty"`
	ContentSHA256 string `json:"content_sha256,omitempty"`
}

func captureActionPreimages(paths []string) ([]byte, error) {
	if len(paths) == 0 {
		return []byte(`[]`), nil
	}
	preimages := make([]actionPreimage, 0, len(paths))
	for _, path := range paths {
		preimage, err := captureActionPreimage(path)
		if err != nil {
			return nil, err
		}
		preimages = append(preimages, preimage)
	}
	return json.Marshal(preimages)
}

func captureActionPreimage(path string) (actionPreimage, error) {
	info, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		return actionPreimage{Path: path}, nil
	}
	if err != nil {
		return actionPreimage{}, fmt.Errorf("capture action preimage %q: %w", path, err)
	}
	preimage := actionPreimage{
		Path:          path,
		Exists:        true,
		Size:          info.Size(),
		Mode:          uint32(info.Mode()),
		ModTimeUnixNS: info.ModTime().UnixNano(),
	}
	if !info.Mode().IsRegular() {
		return preimage, nil
	}
	digest, err := fileSHA256(path)
	if err != nil {
		return actionPreimage{}, fmt.Errorf("capture action preimage %q: %w", path, err)
	}
	after, err := os.Stat(path)
	if err != nil {
		return actionPreimage{}, fmt.Errorf("capture action preimage %q after hashing: %w", path, err)
	}
	if info.Size() != after.Size() ||
		info.Mode() != after.Mode() ||
		!info.ModTime().Equal(after.ModTime()) {
		return actionPreimage{}, fmt.Errorf("file changed while capturing action preimage %q", path)
	}
	preimage.ContentSHA256 = digest
	return preimage, nil
}

func fileSHA256(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	count, err := io.CopyN(hash, file, maxActionPreimageBytes+1)
	if err != nil && !errors.Is(err, io.EOF) {
		return "", err
	}
	if count > maxActionPreimageBytes {
		return "", fmt.Errorf("file exceeds %d-byte preimage limit", maxActionPreimageBytes)
	}
	return "sha256:" + hex.EncodeToString(hash.Sum(nil)), nil
}

func revalidateActionPreimages(record session.ActionRecord) error {
	if len(record.Preimages) == 0 {
		return nil
	}
	var preimages []actionPreimage
	if err := json.Unmarshal(record.Preimages, &preimages); err != nil {
		return fmt.Errorf("decode action preimages: %w", err)
	}
	for _, expected := range preimages {
		if expected.Path == "" {
			return errors.New("action preimage has no path")
		}
		actual, err := captureActionPreimage(expected.Path)
		if err != nil {
			return err
		}
		if !sameActionPreimage(expected, actual) {
			return fmt.Errorf("action preimage changed for %q", expected.Path)
		}
	}
	return nil
}

func sameActionPreimage(expected, actual actionPreimage) bool {
	return expected.Path == actual.Path &&
		expected.Exists == actual.Exists &&
		expected.Size == actual.Size &&
		expected.Mode == actual.Mode &&
		expected.ModTimeUnixNS == actual.ModTimeUnixNS &&
		expected.ContentSHA256 == actual.ContentSHA256
}
