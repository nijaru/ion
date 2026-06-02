package tool

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	ionworkspace "github.com/nijaru/ion/internal/workspace"
	"github.com/nijaru/ion/llm"
)

func newTestFileTool(t *testing.T, cwd string) *FileTool {
	t.Helper()
	return &FileTool{
		cwd:        cwd,
		checkpoint: ionworkspace.NewCheckpointStore(filepath.Join(t.TempDir(), "checkpoints")),
	}
}

func marshalEditArgs(t *testing.T, filePath string, edits ...map[string]any) string {
	t.Helper()
	args, err := json.Marshal(map[string]any{
		"file_path": filePath,
		"edits":     edits,
	})
	if err != nil {
		t.Fatal(err)
	}
	return string(args)
}

func mustDecodeBase64(t *testing.T, encoded string) []byte {
	t.Helper()
	data, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func TestFileTools(t *testing.T) {
	tmpDir := t.TempDir()

	t.Run("Write and Read", func(t *testing.T) {
		w := &Write{FileTool: *newTestFileTool(t, tmpDir)}
		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}

		filePath := "test.txt"
		content := "line 1\nline 2\nline 3"

		// Write
		writeArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"content":   content,
		})
		writeResult, err := w.Execute(context.Background(), string(writeArgs))
		if err != nil {
			t.Fatalf("write failed: %v", err)
		}
		if writeResult != "Wrote test.txt." {
			t.Fatalf("write result = %q, want concise success", writeResult)
		}

		// Read full
		readArgs, _ := json.Marshal(map[string]any{"file_path": filePath})
		res, err := r.Execute(context.Background(), string(readArgs))
		if err != nil {
			t.Fatalf("read failed: %v", err)
		}
		wantRead := "     1\tline 1\n     2\tline 2\n     3\tline 3"
		if res != wantRead {
			t.Errorf("expected %q, got %q", wantRead, res)
		}

		// Pi-style path argument.
		piReadArgs, _ := json.Marshal(map[string]any{"path": filePath, "offset": 2, "limit": 1})
		res, err = r.Execute(context.Background(), string(piReadArgs))
		if err != nil {
			t.Fatalf("read with Pi-style path failed: %v", err)
		}
		if res != "     2\tline 2\n\n[1 more line(s) in file. Use offset=3 to continue.]" {
			t.Errorf("Pi-style read path expected numbered line 2, got %q", res)
		}

		spacePath := "space name.txt"
		if err := os.WriteFile(filepath.Join(tmpDir, spacePath), []byte("space ok"), 0o644); err != nil {
			t.Fatal(err)
		}
		normalizedReadArgs, _ := json.Marshal(map[string]any{"path": "@space\u202fname.txt"})
		res, err = r.Execute(context.Background(), string(normalizedReadArgs))
		if err != nil {
			t.Fatalf("read with @/unicode-space path failed: %v", err)
		}
		if strings.TrimSpace(res) != "1\tspace ok" {
			t.Fatalf("normalized path read = %q, want space ok", res)
		}

		screenshotPath := "Screenshot 1\u202fPM.txt"
		if err := os.WriteFile(filepath.Join(tmpDir, screenshotPath), []byte("screenshot ok"), 0o644); err != nil {
			t.Fatal(err)
		}
		screenshotArgs, _ := json.Marshal(map[string]any{"path": "Screenshot 1 PM.txt"})
		res, err = r.Execute(context.Background(), string(screenshotArgs))
		if err != nil {
			t.Fatalf("read with macOS screenshot AM/PM path failed: %v", err)
		}
		if !strings.Contains(res, "screenshot ok") {
			t.Fatalf("screenshot path read = %q, want screenshot ok", res)
		}

		nfdPath := "Cafe\u0301.txt"
		if err := os.WriteFile(filepath.Join(tmpDir, nfdPath), []byte("nfd ok"), 0o644); err != nil {
			t.Fatal(err)
		}
		nfdArgs, _ := json.Marshal(map[string]any{"path": "Caf\u00e9.txt"})
		res, err = r.Execute(context.Background(), string(nfdArgs))
		if err != nil {
			t.Fatalf("read with NFD path variant failed: %v", err)
		}
		if !strings.Contains(res, "nfd ok") {
			t.Fatalf("NFD path read = %q, want nfd ok", res)
		}

		curlyPath := "Guide\u2019s.txt"
		if err := os.WriteFile(filepath.Join(tmpDir, curlyPath), []byte("curly ok"), 0o644); err != nil {
			t.Fatal(err)
		}
		curlyArgs, _ := json.Marshal(map[string]any{"path": "Guide's.txt"})
		res, err = r.Execute(context.Background(), string(curlyArgs))
		if err != nil {
			t.Fatalf("read with curly quote path variant failed: %v", err)
		}
		if !strings.Contains(res, "curly ok") {
			t.Fatalf("curly path read = %q, want curly ok", res)
		}

		fileURLReadArgs, _ := json.Marshal(map[string]any{
			"path": "file://" + filepath.ToSlash(filepath.Join(tmpDir, filePath)),
		})
		res, err = r.Execute(context.Background(), string(fileURLReadArgs))
		if err != nil {
			t.Fatalf("read with file URL path failed: %v", err)
		}
		if !strings.Contains(res, "line 1") {
			t.Fatalf("file URL read = %q, want file contents", res)
		}
		remoteFileURLReadArgs, _ := json.Marshal(map[string]any{
			"path": "file://example.com/" + filepath.ToSlash(filepath.Join(tmpDir, filePath)),
		})
		if _, err := r.Execute(context.Background(), string(remoteFileURLReadArgs)); err == nil {
			t.Fatal("expected remote file URL host to fail")
		}

		// Read with limit/offset
		limitArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"offset":    2,
			"limit":     1,
		})
		res, err = r.Execute(context.Background(), string(limitArgs))
		if err != nil {
			t.Fatalf("read with limit failed: %v", err)
		}
		if res != "     2\tline 2\n\n[1 more line(s) in file. Use offset=3 to continue.]" {
			t.Errorf("expected numbered line 2, got %q", res)
		}

		zeroOffsetArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"offset":    0,
			"limit":     1,
		})
		res, err = r.Execute(context.Background(), string(zeroOffsetArgs))
		if err != nil {
			t.Fatalf("read with zero offset failed: %v", err)
		}
		if res != "     1\tline 1\n\n[2 more line(s) in file. Use offset=2 to continue.]" {
			t.Errorf("expected zero offset to start at line 1, got %q", res)
		}

		negativeOffsetArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"offset":    -1,
			"limit":     1,
		})
		if _, err := r.Execute(context.Background(), string(negativeOffsetArgs)); err == nil {
			t.Fatal("expected negative offset to fail")
		}

		absArgs, _ := json.Marshal(map[string]any{"file_path": filepath.Join(tmpDir, filePath)})
		res, err = r.Execute(context.Background(), string(absArgs))
		if err != nil {
			t.Fatalf("read with absolute in-workspace path failed: %v", err)
		}
		if res != wantRead {
			t.Errorf("absolute read expected %q, got %q", wantRead, res)
		}

		outside := filepath.Join(t.TempDir(), "outside.txt")
		if err := os.WriteFile(outside, []byte("outside"), 0o644); err != nil {
			t.Fatal(err)
		}
		outsideArgs, _ := json.Marshal(map[string]any{"file_path": outside})
		res, err = r.Execute(context.Background(), string(outsideArgs))
		if err != nil {
			t.Fatalf("read with absolute path outside workspace failed: %v", err)
		}
		if res != "     1\toutside" {
			t.Fatalf("absolute outside read = %q, want numbered outside content", res)
		}

		linkPath := filepath.Join(tmpDir, "outside-link.txt")
		if err := os.Symlink(outside, linkPath); err != nil {
			t.Skipf("symlink unavailable: %v", err)
		}
		linkArgs, _ := json.Marshal(map[string]any{"file_path": "outside-link.txt"})
		res, err = r.Execute(context.Background(), string(linkArgs))
		if err != nil {
			t.Fatalf("read through symlink failed: %v", err)
		}
		if res != "     1\toutside" {
			t.Fatalf("symlink read = %q, want numbered outside content", res)
		}
	})

	t.Run("Read image returns content parts", func(t *testing.T) {
		const encodedPNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII="
		imagePath := "pixel.png"
		if err := os.WriteFile(filepath.Join(tmpDir, imagePath), mustDecodeBase64(t, encodedPNG), 0o644); err != nil {
			t.Fatal(err)
		}

		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}
		parts, err := r.ExecuteContent(context.Background(), `{"path":"pixel.png"}`)
		if err != nil {
			t.Fatalf("read image content failed: %v", err)
		}
		if len(parts) != 2 {
			t.Fatalf("parts = %+v, want text and image", parts)
		}
		if parts[0].Text != "Read image file [image/png]" {
			t.Fatalf("text part = %q", parts[0].Text)
		}
		if parts[1].Type != llm.ContentPartImage ||
			parts[1].MIMEType != "image/png" ||
			parts[1].Data != encodedPNG {
			t.Fatalf("image part = %+v", parts[1])
		}
		text, err := r.Execute(context.Background(), `{"path":"pixel.png"}`)
		if err != nil {
			t.Fatalf("read image text failed: %v", err)
		}
		if text != "Read image file [image/png]" {
			t.Fatalf("text fallback = %q", text)
		}
	})

	t.Run("List supports sorted limited directory output", func(t *testing.T) {
		dir := filepath.Join(tmpDir, "list")
		if err := os.MkdirAll(filepath.Join(dir, "Beta"), 0o755); err != nil {
			t.Fatal(err)
		}
		for _, name := range []string{"zeta.txt", "Alpha.txt"} {
			if err := os.WriteFile(filepath.Join(dir, name), []byte(name), 0o644); err != nil {
				t.Fatal(err)
			}
		}

		l := &List{FileTool: *newTestFileTool(t, tmpDir)}
		res, err := l.Execute(context.Background(), `{"path":"list","limit":2}`)
		if err != nil {
			t.Fatalf("list failed: %v", err)
		}
		if !strings.HasPrefix(res, "Alpha.txt\nBeta/\n") {
			t.Fatalf("list output = %q, want case-insensitive sorted entries", res)
		}
		if !strings.Contains(res, "2 entries limit reached") {
			t.Fatalf("list output = %q, want limit notice", res)
		}

		emptyDir := filepath.Join(tmpDir, "empty")
		if err := os.MkdirAll(emptyDir, 0o755); err != nil {
			t.Fatal(err)
		}
		res, err = l.Execute(context.Background(), `{"path":"empty"}`)
		if err != nil {
			t.Fatalf("empty list failed: %v", err)
		}
		if strings.TrimSpace(res) != "(empty directory)" {
			t.Fatalf("empty list = %q, want empty directory notice", res)
		}

		if _, err := l.Execute(context.Background(), `{"path":"list/Alpha.txt"}`); err == nil {
			t.Fatal("expected listing a file to fail")
		}
	})

	t.Run("Read line numbering handles trailing newline and empty ranges", func(t *testing.T) {
		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "numbered.txt"
		if err := os.WriteFile(filepath.Join(tmpDir, filePath), []byte("alpha\n\nomega\n"), 0o644); err != nil {
			t.Fatal(err)
		}

		readArgs, _ := json.Marshal(map[string]any{"file_path": filePath})
		res, err := r.Execute(context.Background(), string(readArgs))
		if err != nil {
			t.Fatalf("read failed: %v", err)
		}
		want := "     1\talpha\n     2\t\n     3\tomega"
		if res != want {
			t.Fatalf("read = %q, want %q", res, want)
		}

		emptyRangeArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"offset":    99,
			"limit":     10,
		})
		_, err = r.Execute(context.Background(), string(emptyRangeArgs))
		if err == nil || !strings.Contains(err.Error(), "beyond end of file") {
			t.Fatalf("empty range read error = %v, want beyond end of file", err)
		}
	})

	t.Run("Read applies Pi-style default continuation limits", func(t *testing.T) {
		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "long-read.txt"
		lines := make([]string, 2002)
		for i := range lines {
			lines[i] = fmt.Sprintf("line %d", i+1)
		}
		if err := os.WriteFile(filepath.Join(tmpDir, filePath), []byte(strings.Join(lines, "\n")), 0o644); err != nil {
			t.Fatal(err)
		}

		res, err := r.Execute(context.Background(), `{"path":"long-read.txt"}`)
		if err != nil {
			t.Fatalf("long read failed: %v", err)
		}
		if !strings.Contains(res, "  2000\tline 2000") {
			t.Fatalf("long read missing line 2000: %q", res)
		}
		if strings.Contains(res, "line 2001") {
			t.Fatalf("long read included line beyond default limit: %q", res)
		}
		if !strings.Contains(
			res,
			"[Showing lines 1-2000 of 2002 (2000 line limit). Use offset=2001 to continue.]",
		) {
			t.Fatalf("long read missing line-limit continuation notice: %q", res)
		}
	})

	t.Run("Read applies Pi-style byte continuation limits", func(t *testing.T) {
		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "wide-read.txt"
		content := strings.Repeat("a", MaxToolOutputSize/2) + "\n" +
			strings.Repeat("b", MaxToolOutputSize/2)
		if err := os.WriteFile(filepath.Join(tmpDir, filePath), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}

		res, err := r.Execute(context.Background(), `{"path":"wide-read.txt"}`)
		if err != nil {
			t.Fatalf("wide read failed: %v", err)
		}
		if strings.Contains(res, "\tb") {
			t.Fatalf("wide read included line beyond byte limit: %q", res)
		}
		if !strings.Contains(res, fmt.Sprintf("(%d byte limit)", MaxToolOutputSize)) {
			t.Fatalf("wide read missing byte-limit continuation notice: %q", res)
		}
		if !strings.Contains(res, "Use offset=2 to continue.") {
			t.Fatalf("wide read missing byte-limit offset: %q", res)
		}
	})

	t.Run("Read reports oversized first line", func(t *testing.T) {
		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "huge-line.txt"
		if err := os.WriteFile(
			filepath.Join(tmpDir, filePath),
			[]byte(strings.Repeat("x", MaxToolOutputSize)),
			0o644,
		); err != nil {
			t.Fatal(err)
		}

		res, err := r.Execute(context.Background(), `{"path":"huge-line.txt"}`)
		if err != nil {
			t.Fatalf("huge line read failed: %v", err)
		}
		if !strings.Contains(res, fmt.Sprintf("Line 1 exceeds %d bytes", MaxToolOutputSize)) {
			t.Fatalf("huge line read missing oversized-line notice: %q", res)
		}
	})

	t.Run("Write supports trusted absolute paths and symlinks", func(t *testing.T) {
		w := &Write{FileTool: *newTestFileTool(t, tmpDir)}
		outsideDir := t.TempDir()
		outsideFile := filepath.Join(outsideDir, "outside.txt")
		if err := os.WriteFile(outsideFile, []byte("outside"), 0o644); err != nil {
			t.Fatal(err)
		}
		absoluteArgs, _ := json.Marshal(map[string]any{
			"file_path": filepath.Join(outsideDir, "absolute.txt"),
			"content":   "absolute",
		})
		if _, err := w.Execute(context.Background(), string(absoluteArgs)); err != nil {
			t.Fatalf("write absolute path outside workspace failed: %v", err)
		}
		data, err := os.ReadFile(filepath.Join(outsideDir, "absolute.txt"))
		if err != nil {
			t.Fatal(err)
		}
		if string(data) != "absolute" {
			t.Fatalf("absolute file = %q, want absolute", data)
		}

		aliasArgs, _ := json.Marshal(map[string]any{
			"path":    "pi-write.txt",
			"content": "pi path",
		})
		if _, err := w.Execute(context.Background(), string(aliasArgs)); err != nil {
			t.Fatalf("write with Pi-style path failed: %v", err)
		}
		data, err = os.ReadFile(filepath.Join(tmpDir, "pi-write.txt"))
		if err != nil {
			t.Fatal(err)
		}
		if string(data) != "pi path" {
			t.Fatalf("Pi-style write path file = %q, want pi path", data)
		}

		if err := os.Symlink(outsideFile, filepath.Join(tmpDir, "outside-write-link.txt")); err != nil {
			t.Skipf("symlink unavailable: %v", err)
		}

		linkArgs, _ := json.Marshal(map[string]any{
			"file_path": "outside-write-link.txt",
			"content":   "changed",
		})
		if _, err := w.Execute(context.Background(), string(linkArgs)); err != nil {
			t.Fatalf("write through symlink failed: %v", err)
		}

		data, err = os.ReadFile(outsideFile)
		if err != nil {
			t.Fatal(err)
		}
		if string(data) != "changed" {
			t.Fatalf("outside file changed to %q", data)
		}

		if err := os.Symlink(outsideDir, filepath.Join(tmpDir, "outside-write-dir")); err != nil {
			t.Skipf("symlink directory unavailable: %v", err)
		}
		dirArgs, _ := json.Marshal(map[string]any{
			"file_path": "outside-write-dir/new.txt",
			"content":   "changed",
		})
		if _, err := w.Execute(context.Background(), string(dirArgs)); err != nil {
			t.Fatalf("write through symlink directory failed: %v", err)
		}
		data, err = os.ReadFile(filepath.Join(outsideDir, "new.txt"))
		if err != nil {
			t.Fatal(err)
		}
		if string(data) != "changed" {
			t.Fatalf("outside dir file = %q, want changed", data)
		}
	})

	t.Run("Write preserves existing file mode", func(t *testing.T) {
		w := &Write{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "script.sh"
		absPath := filepath.Join(tmpDir, filePath)
		if err := os.WriteFile(absPath, []byte("#!/bin/sh\necho before\n"), 0o755); err != nil {
			t.Fatal(err)
		}

		args, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"content":   "#!/bin/sh\necho after\n",
		})
		if _, err := w.Execute(context.Background(), string(args)); err != nil {
			t.Fatalf("write executable: %v", err)
		}
		info, err := os.Stat(absPath)
		if err != nil {
			t.Fatal(err)
		}
		if got := info.Mode().Perm(); got != 0o755 {
			t.Fatalf("mode = %#o, want 0755", got)
		}
	})

	t.Run("Read and Edit handle CRLF and BOM", func(t *testing.T) {
		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "windows.txt"
		original := "\ufeffalpha\r\nbeta\r\n"
		if err := os.WriteFile(filepath.Join(tmpDir, filePath), []byte(original), 0o644); err != nil {
			t.Fatal(err)
		}

		readArgs, _ := json.Marshal(map[string]any{"file_path": filePath})
		res, err := r.Execute(context.Background(), string(readArgs))
		if err != nil {
			t.Fatalf("read failed: %v", err)
		}
		wantRead := "     1\talpha\n     2\tbeta"
		if res != wantRead {
			t.Fatalf("read = %q, want %q", res, wantRead)
		}

		editArgs := marshalEditArgs(t, filePath, map[string]any{
			"old_string": "alpha\nbeta",
			"new_string": "one\ntwo",
		})
		if _, err := e.Execute(context.Background(), editArgs); err != nil {
			t.Fatalf("edit failed: %v", err)
		}
		data, err := os.ReadFile(filepath.Join(tmpDir, filePath))
		if err != nil {
			t.Fatal(err)
		}
		wantFile := "\ufeffone\r\ntwo\r\n"
		if string(data) != wantFile {
			t.Fatalf("edited file = %q, want %q", string(data), wantFile)
		}
	})

	t.Run("Edit", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "edit-test.txt"
		content := "foo\nbar\nbaz"
		os.WriteFile(filepath.Join(tmpDir, filePath), []byte(content), 0o644)

		// Replace unique
		editArgs := marshalEditArgs(t, filePath, map[string]any{
			"oldText": "bar",
			"newText": "qux",
		})
		_, err := e.Execute(context.Background(), editArgs)
		if err != nil {
			t.Fatalf("edit failed: %v", err)
		}

		newContent, _ := os.ReadFile(filepath.Join(tmpDir, filePath))
		if string(newContent) != "foo\nqux\nbaz" {
			t.Errorf("unexpected content: %q", string(newContent))
		}

		executable := "script.sh"
		executablePath := filepath.Join(tmpDir, executable)
		os.WriteFile(executablePath, []byte("#!/bin/sh\necho before\n"), 0o755)
		modeArgs := marshalEditArgs(t, executable, map[string]any{
			"old_string": "before",
			"new_string": "after",
		})
		if _, err := e.Execute(context.Background(), modeArgs); err != nil {
			t.Fatalf("edit executable failed: %v", err)
		}
		info, err := os.Stat(executablePath)
		if err != nil {
			t.Fatal(err)
		}
		if got := info.Mode().Perm(); got != 0o755 {
			t.Fatalf("edit changed file mode to %v, want 0755", got)
		}
		matches, err := filepath.Glob(filepath.Join(tmpDir, ".script.sh.*.tmp"))
		if err != nil {
			t.Fatal(err)
		}
		if len(matches) != 0 {
			t.Fatalf("edit left temporary files: %#v", matches)
		}

		// Fail on non-unique without replace_all
		os.WriteFile(filepath.Join(tmpDir, filePath), []byte("aa\naa"), 0o644)
		failArgs := marshalEditArgs(t, filePath, map[string]any{
			"old_string": "aa",
			"new_string": "bb",
		})
		_, err = e.Execute(context.Background(), failArgs)
		if err == nil {
			t.Error("expected error for non-unique match, got nil")
		}
		if !strings.Contains(err.Error(), "line(s) 1, 2") {
			t.Fatalf("non-unique error = %q, want line numbers", err)
		}

		// Succeed on non-unique with replace_all
		allArgs := marshalEditArgs(t, filePath, map[string]any{
			"old_string":            "aa",
			"new_string":            "bb",
			"replace_all":           true,
			"expected_replacements": 2,
		})
		_, err = e.Execute(context.Background(), allArgs)
		if err != nil {
			t.Fatalf("edit all failed: %v", err)
		}
		newContent, _ = os.ReadFile(filepath.Join(tmpDir, filePath))
		if string(newContent) != "bb\nbb" {
			t.Errorf("unexpected content: %q", string(newContent))
		}

		os.WriteFile(filepath.Join(tmpDir, filePath), []byte("xx\nxx"), 0o644)
		wrongExpectedArgs := marshalEditArgs(t, filePath, map[string]any{
			"old_string":            "xx",
			"new_string":            "yy",
			"replace_all":           true,
			"expected_replacements": 1,
		})
		if _, err := e.Execute(context.Background(), wrongExpectedArgs); err == nil ||
			!strings.Contains(err.Error(), "expected 1 replacement(s)") ||
			!strings.Contains(err.Error(), "line(s) 1, 2") {
			t.Fatalf("expected replacement-count error with line numbers, got %v", err)
		}

		emptyOldArgs := marshalEditArgs(t, filePath, map[string]any{
			"old_string": "",
			"new_string": "x",
		})
		if _, err := e.Execute(context.Background(), emptyOldArgs); err == nil {
			t.Fatal("expected empty oldText to fail")
		}

		noopArgs := marshalEditArgs(t, filePath, map[string]any{
			"old_string": "bb",
			"new_string": "bb",
		})
		if _, err := e.Execute(context.Background(), noopArgs); err == nil {
			t.Fatal("expected no-op edit to fail")
		}
	})

	t.Run("Edit supports trusted symlinks", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		outside := filepath.Join(t.TempDir(), "outside-edit.txt")
		if err := os.WriteFile(outside, []byte("outside\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(outside, filepath.Join(tmpDir, "outside-edit-link.txt")); err != nil {
			t.Skipf("symlink unavailable: %v", err)
		}

		args := marshalEditArgs(t, "outside-edit-link.txt", map[string]any{
			"old_string": "outside",
			"new_string": "changed",
		})
		if _, err := e.Execute(context.Background(), args); err != nil {
			t.Fatalf("edit through symlink failed: %v", err)
		}

		data, err := os.ReadFile(outside)
		if err != nil {
			t.Fatal(err)
		}
		if string(data) != "changed\n" {
			t.Fatalf("outside file changed to %q", data)
		}
	})

	t.Run("Edit multiple replacements", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}

		f1 := "file1.txt"
		os.WriteFile(filepath.Join(tmpDir, f1), []byte("hello\nworld\n"), 0o755)
		if err := os.Chmod(filepath.Join(tmpDir, f1), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(tmpDir, f1+".tmp"), []byte("user temp"), 0o644); err != nil {
			t.Fatal(err)
		}

		args := marshalEditArgs(
			t,
			f1,
			map[string]any{
				"old_string": "world",
				"new_string": "ion",
			},
			map[string]any{
				"old_string": "hello",
				"new_string": "hi",
			},
		)

		res, err := e.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("edit failed: %v", err)
		}
		if strings.Contains(res, "Checkpoint: ") {
			t.Fatalf("edit result leaked checkpoint id: %q", res)
		}
		if !strings.Contains(res, "Applied 2 edit(s) with 2 replacement(s) in file1.txt.") {
			t.Fatalf("edit result = %q, want concise success", res)
		}

		// Verify content
		c1, _ := os.ReadFile(filepath.Join(tmpDir, f1))
		if string(c1) != "hi\nion\n" {
			t.Errorf("f1 content mismatch: %q", string(c1))
		}

		// Verify diff output
		if !strings.Contains(res, "--- a/file1.txt") || !strings.Contains(res, "+++ b/file1.txt") {
			t.Errorf("diff for f1 missing in result: %q", res)
		}
		if !strings.Contains(res, "-hello") || !strings.Contains(res, "+hi") ||
			!strings.Contains(res, "-world") || !strings.Contains(res, "+ion") {
			t.Errorf("hunk for f1 missing in result: %q", res)
		}
		f1Info, err := os.Stat(filepath.Join(tmpDir, f1))
		if err != nil {
			t.Fatal(err)
		}
		if got := f1Info.Mode().Perm(); got != 0o755 {
			t.Fatalf("edit changed file mode to %v, want 0755", got)
		}
		tempContent, err := os.ReadFile(filepath.Join(tmpDir, f1+".tmp"))
		if err != nil {
			t.Fatalf("user temp file missing after edit: %v", err)
		}
		if string(tempContent) != "user temp" {
			t.Fatalf("user temp file = %q, want preserved", tempContent)
		}

		emptyArgs, _ := json.Marshal(map[string]any{"file_path": f1, "edits": []map[string]any{}})
		if _, err := e.Execute(context.Background(), string(emptyArgs)); err == nil {
			t.Fatal("expected empty edits to fail")
		}

		badArgs := marshalEditArgs(t, f1, map[string]any{
			"old_string": "",
			"new_string": "x",
		})
		if _, err := e.Execute(context.Background(), badArgs); err == nil {
			t.Fatal("expected edit with empty old_string to fail")
		}

		ambiguousArgs := marshalEditArgs(t, f1, map[string]any{
			"old_string": "i",
			"new_string": "O",
		})
		if _, err := e.Execute(context.Background(), ambiguousArgs); err == nil ||
			!strings.Contains(err.Error(), "line(s)") {
			t.Fatalf("expected ambiguous edit error with line numbers, got %v", err)
		}
	})

	t.Run("Edit multiple replacements supports trusted symlinks", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		outside := filepath.Join(t.TempDir(), "outside-multi-edit.txt")
		if err := os.WriteFile(outside, []byte("outside\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(outside, filepath.Join(tmpDir, "outside-multi-edit-link.txt")); err != nil {
			t.Skipf("symlink unavailable: %v", err)
		}

		args := marshalEditArgs(t, "outside-multi-edit-link.txt", map[string]any{
			"old_string": "outside",
			"new_string": "changed",
		})
		if _, err := e.Execute(context.Background(), args); err != nil {
			t.Fatalf("edit through symlink failed: %v", err)
		}

		outsideData, err := os.ReadFile(outside)
		if err != nil {
			t.Fatal(err)
		}
		if string(outsideData) != "changed\n" {
			t.Fatalf("outside file changed to %q", outsideData)
		}
	})

	t.Run("List", func(t *testing.T) {
		l := &List{FileTool: *newTestFileTool(t, tmpDir)}
		os.Mkdir(filepath.Join(tmpDir, "subdir"), 0o755)
		os.WriteFile(filepath.Join(tmpDir, "file.txt"), []byte("hi"), 0o644)

		args := `{"path": "."}`
		res, err := l.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("list failed: %v", err)
		}

		if !strings.Contains(res, "subdir/") {
			t.Errorf("expected list to contain subdir/, got %q", res)
		}
		if !strings.Contains(res, "file.txt") {
			t.Errorf("expected list to contain file.txt, got %q", res)
		}

		if _, err := l.Execute(context.Background(), `{"path":`); err == nil {
			t.Fatal("expected invalid list JSON to fail")
		}

		absArgs := `{"path":` + strconv.Quote(tmpDir) + `}`
		if _, err := l.Execute(context.Background(), absArgs); err != nil {
			t.Fatalf("list with absolute workspace path failed: %v", err)
		}

		outsideDir := t.TempDir()
		if err := os.WriteFile(filepath.Join(outsideDir, "outside.txt"), []byte("outside"), 0o644); err != nil {
			t.Fatal(err)
		}
		absOutsideArgs := `{"path":` + strconv.Quote(outsideDir) + `}`
		res, err = l.Execute(context.Background(), absOutsideArgs)
		if err != nil {
			t.Fatalf("list with absolute outside path failed: %v", err)
		}
		if !strings.Contains(res, "outside.txt") {
			t.Fatalf("absolute outside list = %q, want outside.txt", res)
		}
		if err := os.Symlink(outsideDir, filepath.Join(tmpDir, "outside-dir-link")); err != nil {
			t.Skipf("symlink unavailable: %v", err)
		}
		res, err = l.Execute(context.Background(), `{"path":"outside-dir-link"}`)
		if err != nil {
			t.Fatalf("list through symlink directory failed: %v", err)
		}
		if !strings.Contains(res, "outside.txt") {
			t.Fatalf("symlink directory list = %q, want outside.txt", res)
		}
	})
}
