package session

import (
	"context"
	"path/filepath"
	"testing"
	"time"
)

func TestSQLiteFTSFullTextSearch(t *testing.T) {
	tempDir := t.TempDir()
	dbPath := filepath.Join(tempDir, "fts-test.db")
	store, err := NewSQLiteStore(dbPath, "fts-session-1")
	if err != nil {
		t.Fatalf("NewSQLiteStore failed: %v", err)
	}
	defer store.Close()

	sess := NewSession(store, 16)
	ctx := context.Background()
	now := time.Now()

	// Append various messages with distinct keywords
	_, err = sess.AppendMessage(ctx, NewUserText("How do I configure Postgres database connection pooling in Go?", now))
	if err != nil {
		t.Fatalf("AppendMessage 1 failed: %v", err)
	}

	_, err = sess.AppendMessage(ctx, &AssistantMessage{
		Content: []Content{
			TextContent{Text: "You can use pgxpool with max connections set to 25 and health check timeouts."},
		},
		Timestamp: now.Add(time.Second),
	})
	if err != nil {
		t.Fatalf("AppendMessage 2 failed: %v", err)
	}

	_, err = sess.AppendMessage(ctx, NewUserText("What about SQLite WAL mode concurrency?", now.Add(2*time.Second)))
	if err != nil {
		t.Fatalf("AppendMessage 3 failed: %v", err)
	}

	// Search for "Postgres connection"
	results, err := sess.SearchEntries(ctx, "Postgres connection", 10)
	if err != nil {
		t.Fatalf("SearchEntries('Postgres connection') failed: %v", err)
	}
	if len(results) == 0 {
		t.Fatal("expected search results for 'Postgres connection', got 0")
	}
	if results[0].Role != "user" {
		t.Fatalf("expected role 'user', got %q", results[0].Role)
	}

	// Search for "pgxpool"
	results2, err := sess.SearchEntries(ctx, "pgxpool", 10)
	if err != nil {
		t.Fatalf("SearchEntries('pgxpool') failed: %v", err)
	}
	if len(results2) == 0 {
		t.Fatal("expected search results for 'pgxpool', got 0")
	}
	if results2[0].Role != "assistant" {
		t.Fatalf("expected role 'assistant', got %q", results2[0].Role)
	}

	// Search for "SQLite WAL"
	results3, err := sess.SearchEntries(ctx, "SQLite WAL", 10)
	if err != nil {
		t.Fatalf("SearchEntries('SQLite WAL') failed: %v", err)
	}
	if len(results3) == 0 {
		t.Fatal("expected search results for 'SQLite WAL', got 0")
	}

	// Search for non-existent term
	results4, err := sess.SearchEntries(ctx, "nonexistentxyzterm", 10)
	if err != nil {
		t.Fatalf("SearchEntries for nonexistent term failed: %v", err)
	}
	if len(results4) != 0 {
		t.Fatalf("expected 0 results, got %d", len(results4))
	}
}

func TestSQLiteFTSEdgeCasesAndPunctuation(t *testing.T) {
	tempDir := t.TempDir()
	dbPath := filepath.Join(tempDir, "fts-edge-test.db")
	store, err := NewSQLiteStore(dbPath, "fts-edge-session")
	if err != nil {
		t.Fatalf("NewSQLiteStore failed: %v", err)
	}
	defer store.Close()

	sess := NewSession(store, 16)
	ctx := context.Background()
	now := time.Now()

	// Append unicode and multilingual messages
	_, err = sess.AppendMessage(ctx, NewUserText("東京のサーバー設定とデータベース", now))
	if err != nil {
		t.Fatal(err)
	}
	_, err = sess.AppendMessage(ctx, NewUserText("Конфигурация сервера в облаке", now.Add(time.Second)))
	if err != nil {
		t.Fatal(err)
	}
	_, err = sess.AppendMessage(
		ctx,
		NewUserText("Special characters: {key: 'value'}, [1, 2, 3], &^%$#@!*()", now.Add(2*time.Second)),
	)
	if err != nil {
		t.Fatal(err)
	}

	// 1. Empty and whitespace queries
	for _, query := range []string{"", "   ", "\t\n"} {
		res, err := sess.SearchEntries(ctx, query, 10)
		if err != nil {
			t.Fatalf("expected nil error for empty query %q, got: %v", query, err)
		}
		if len(res) != 0 {
			t.Fatalf("expected 0 results for empty query %q, got %d", query, len(res))
		}
	}

	// 2. Heavy punctuation & Boolean operators in user input
	punctuations := []string{
		`"unclosed quote`,
		`:::colons:::`,
		`AND NOT OR NEAR(a, b)`,
		`{key: 'value'}`,
		`*wildcard*`,
		`()^^^`,
	}
	for _, p := range punctuations {
		// Must not panic or fail with SQLite syntax error
		_, err := sess.SearchEntries(ctx, p, 10)
		if err != nil {
			t.Fatalf("search with punctuation %q failed with error: %v", p, err)
		}
	}

	// 3. Unicode keyword matching
	res, err := sess.SearchEntries(ctx, "東京", 10)
	if err != nil {
		t.Fatalf("search for '東京' failed: %v", err)
	}
	if len(res) == 0 {
		t.Fatal("expected match for '東京'")
	}

	res, err = sess.SearchEntries(ctx, "Конфигурация", 10)
	if err != nil {
		t.Fatalf("search for 'Конфигурация' failed: %v", err)
	}
	if len(res) == 0 {
		t.Fatal("expected match for 'Конфигурация'")
	}

	// 4. Closed store returns ErrSessionClosed
	closedStore, _ := NewSQLiteStore(filepath.Join(tempDir, "closed.db"), "closed-session")
	closedStore.Close()
	_, err = closedStore.SearchEntries(ctx, "test", 10)
	if err != ErrSessionClosed {
		t.Fatalf("expected ErrSessionClosed on closed store, got: %v", err)
	}
}

func BenchmarkSQLiteFTSSearch256(b *testing.B) {
	tempDir := b.TempDir()
	dbPath := filepath.Join(tempDir, "fts-bench.db")
	store, err := NewSQLiteStore(dbPath, "fts-bench-session")
	if err != nil {
		b.Fatalf("NewSQLiteStore failed: %v", err)
	}
	defer store.Close()

	sess := NewSession(store, 16)
	ctx := context.Background()
	now := time.Now()

	for i := 0; i < 256; i++ {
		_, _ = sess.AppendMessage(
			ctx,
			NewUserText(
				"How do I configure Postgres database connection pooling and SQLite WAL in Go?",
				now.Add(time.Duration(i)*time.Second),
			),
		)
	}

	b.ResetTimer()
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		res, err := sess.SearchEntries(ctx, "Postgres connection", 10)
		if err != nil || len(res) == 0 {
			b.Fatalf("search failed: %v", err)
		}
	}
}
