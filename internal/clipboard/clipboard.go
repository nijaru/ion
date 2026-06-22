// Package clipboard provides clipboard image reading functionality.
package clipboard

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// ImageData represents an image read from the clipboard.
type ImageData struct {
	Bytes    []byte
	MimeType string
	FilePath string // Path to temp file where image was saved
}

// ReadClipboardImage reads an image from the system clipboard.
// Returns nil if no image is available or clipboard access fails.
func ReadClipboardImage() (*ImageData, error) {
	switch runtime.GOOS {
	case "darwin":
		return readMacOSClipboard()
	case "linux":
		return readLinuxClipboard()
	default:
		return nil, fmt.Errorf("unsupported platform: %s", runtime.GOOS)
	}
}

// readMacOSClipboard reads an image from macOS clipboard using osascript.
func readMacOSClipboard() (*ImageData, error) {
	// Use osascript to read clipboard image
	script := `
	set imgData to the clipboard as «class PNGf»
	return imgData
`
	cmd := exec.Command("osascript", "-e", script)
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	
	if err := cmd.Run(); err != nil {
		// Try alternative method with pngpaste
		return readMacOSClipboardAlt()
	}
	
	data := stdout.Bytes()
	if len(data) == 0 {
		return nil, fmt.Errorf("no image in clipboard")
	}
	
	// Save to temp file
	tmpFile, err := saveToTempFile(data, "png")
	if err != nil {
		return nil, fmt.Errorf("failed to save temp file: %w", err)
	}
	
	return &ImageData{
		Bytes:    data,
		MimeType: "image/png",
		FilePath: tmpFile,
	}, nil
}

// readMacOSClipboardAlt uses pngpaste as fallback on macOS.
func readMacOSClipboardAlt() (*ImageData, error) {
	// Check if pngpaste is installed
	if _, err := exec.LookPath("pngpaste"); err != nil {
		return readMacOSClipboardNative()
	}
	
	tmpFile := filepath.Join(os.TempDir(), fmt.Sprintf("ion-clipboard-%s.png", randomID()))
	cmd := exec.Command("pngpaste", tmpFile)
	
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("pngpaste failed: %w", err)
	}
	
	data, err := os.ReadFile(tmpFile)
	if err != nil {
		return nil, fmt.Errorf("failed to read temp file: %w", err)
	}
	
	if len(data) == 0 {
		os.Remove(tmpFile)
		return nil, fmt.Errorf("no image in clipboard")
	}
	
	return &ImageData{
		Bytes:    data,
		MimeType: "image/png",
		FilePath: tmpFile,
	}, nil
}

// readMacOSClipboardNative uses Swift for native clipboard access.
func readMacOSClipboardNative() (*ImageData, error) {
	// Use a small Swift program to read clipboard
	script := `
import Cocoa
import Foundation

let pasteboard = NSPasteboard.general
if let data = pasteboard.data(forType: .png) {
    FileHandle.standardOutput.write(data)
}
`
	tmpSwift := filepath.Join(os.TempDir(), "ion-clipboard.swift")
	if err := os.WriteFile(tmpSwift, []byte(script), 0600); err != nil {
		return nil, fmt.Errorf("failed to write swift script: %w", err)
	}
	defer os.Remove(tmpSwift)
	
	cmd := exec.Command("swift", tmpSwift)
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("swift clipboard read failed: %w", err)
	}
	
	data := stdout.Bytes()
	if len(data) == 0 {
		return nil, fmt.Errorf("no image in clipboard")
	}
	
	tmpFile, err := saveToTempFile(data, "png")
	if err != nil {
		return nil, fmt.Errorf("failed to save temp file: %w", err)
	}
	
	return &ImageData{
		Bytes:    data,
		MimeType: "image/png",
		FilePath: tmpFile,
	}, nil
}

// readLinuxClipboard reads an image from Linux clipboard.
func readLinuxClipboard() (*ImageData, error) {
	// Check for Wayland
	if os.Getenv("WAYLAND_DISPLAY") != "" || os.Getenv("XDG_SESSION_TYPE") == "wayland" {
		return readWaylandClipboard()
	}
	
	// Try xclip for X11
	return readX11Clipboard()
}

// readWaylandClipboard reads clipboard using wl-paste.
func readWaylandClipboard() (*ImageData, error) {
	// List available types
	cmd := exec.Command("wl-paste", "--list-types")
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	
	if err := cmd.Run(); err != nil {
		// Fall back to xclip
		return readX11Clipboard()
	}
	
	types := strings.Split(stdout.String(), "\n")
	mimeType := selectImageType(types)
	if mimeType == "" {
		return readX11Clipboard()
	}
	
	// Read image data
	cmd = exec.Command("wl-paste", "--type", mimeType, "--no-newline")
	stdout.Reset()
	cmd.Stdout = &stdout
	
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("wl-paste failed: %w", err)
	}
	
	data := stdout.Bytes()
	if len(data) == 0 {
		return nil, fmt.Errorf("no image in clipboard")
	}
	
	ext := extensionForMimeType(mimeType)
	tmpFile, err := saveToTempFile(data, ext)
	if err != nil {
		return nil, fmt.Errorf("failed to save temp file: %w", err)
	}
	
	return &ImageData{
		Bytes:    data,
		MimeType: mimeType,
		FilePath: tmpFile,
	}, nil
}

// readX11Clipboard reads clipboard using xclip.
func readX11Clipboard() (*ImageData, error) {
	// Try common image types
	imageTypes := []string{"image/png", "image/jpeg", "image/webp", "image/gif"}
	
	for _, mimeType := range imageTypes {
		cmd := exec.Command("xclip", "-selection", "clipboard", "-t", mimeType, "-o")
		var stdout bytes.Buffer
		cmd.Stdout = &stdout
		
		if err := cmd.Run(); err != nil {
			continue
		}
		
		data := stdout.Bytes()
		if len(data) == 0 {
			continue
		}
		
		ext := extensionForMimeType(mimeType)
		tmpFile, err := saveToTempFile(data, ext)
		if err != nil {
			return nil, fmt.Errorf("failed to save temp file: %w", err)
		}
		
		return &ImageData{
			Bytes:    data,
			MimeType: mimeType,
			FilePath: tmpFile,
		}, nil
	}
	
	return nil, fmt.Errorf("no image in clipboard")
}

// selectImageType selects the best image MIME type from available types.
func selectImageType(types []string) string {
	preferred := []string{"image/png", "image/jpeg", "image/webp", "image/gif"}
	
	for _, p := range preferred {
		for _, t := range types {
			t = strings.TrimSpace(t)
			if strings.HasPrefix(t, ";") {
				t = strings.TrimSpace(t[1:])
			}
			if t == p {
				return t
			}
		}
	}
	
	// Try any image type
	for _, t := range types {
		t = strings.TrimSpace(t)
		if strings.HasPrefix(t, "image/") {
			return t
		}
	}
	
	return ""
}

// extensionForMimeType returns the file extension for a MIME type.
func extensionForMimeType(mimeType string) string {
	switch strings.Split(mimeType, ";")[0] {
	case "image/png":
		return "png"
	case "image/jpeg":
		return "jpg"
	case "image/webp":
		return "webp"
	case "image/gif":
		return "gif"
	default:
		return "png"
	}
}

// saveToTempFile saves data to a temporary file.
func saveToTempFile(data []byte, ext string) (string, error) {
	tmpFile := filepath.Join(os.TempDir(), fmt.Sprintf("ion-clipboard-%s.%s", randomID(), ext))
	if err := os.WriteFile(tmpFile, data, 0600); err != nil {
		return "", err
	}
	return tmpFile, nil
}

// randomID generates a random ID for temp file names.
func randomID() string {
	b := make([]byte, 8)
	rand.Read(b)
	return hex.EncodeToString(b)
}

// WriteClipboardText writes text to the system clipboard.
func WriteClipboardText(text string) error {
	switch runtime.GOOS {
	case "darwin":
		return writeMacOSClipboard(text)
	case "linux":
		return writeLinuxClipboard(text)
	default:
		return fmt.Errorf("unsupported platform: %s", runtime.GOOS)
	}
}

// writeMacOSClipboard writes text to macOS clipboard using pbcopy.
func writeMacOSClipboard(text string) error {
	cmd := exec.Command("pbcopy")
	cmd.Stdin = strings.NewReader(text)
	return cmd.Run()
}

// writeLinuxClipboard writes text to Linux clipboard.
func writeLinuxClipboard(text string) error {
	// Check for Wayland
	if os.Getenv("WAYLAND_DISPLAY") != "" || os.Getenv("XDG_SESSION_TYPE") == "wayland" {
		return writeWaylandClipboard(text)
	}
	// Try xclip for X11
	return writeX11Clipboard(text)
}

// writeWaylandClipboard writes text using wl-copy.
func writeWaylandClipboard(text string) error {
	cmd := exec.Command("wl-copy")
	cmd.Stdin = strings.NewReader(text)
	return cmd.Run()
}

// writeX11Clipboard writes text using xclip.
func writeX11Clipboard(text string) error {
	cmd := exec.Command("xclip", "-selection", "clipboard")
	cmd.Stdin = strings.NewReader(text)
	return cmd.Run()
}
