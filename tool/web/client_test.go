package web

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func testClient(serverURL string) *Client {
	return NewClient(Config{
		SearchURL:         serverURL,
		AllowPrivateHosts: true,
		Timeout:           time.Second,
		Now:               func() time.Time { return time.Date(2026, 8, 10, 12, 0, 0, 0, time.UTC) },
	})
}

func TestClientSearchParsesBoundedSources(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if got := r.URL.Query().Get("q"); got != "ion runtime" {
			t.Fatalf("query = %q", got)
		}
		fmt.Fprint(w, `<html><body>
<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fion">Ion runtime</a><a class="result__snippet">bounded runtime design</a></div>
<div class="result"><a class="result__a" href="https://example.org/tui#section">Ion TUI</a><a class="result__snippet">terminal interface</a></div>
</body></html>`)
	}))
	defer server.Close()

	client := testClient(server.URL)
	content, details, err := client.search(context.Background(), searchRequest{Query: "ion runtime", MaxResults: 2})
	if err != nil {
		t.Fatalf("search error = %v", err)
	}
	if len(details.Sources) != 2 || details.Sources[0].URL != "https://example.com/ion" {
		t.Fatalf("sources = %#v", details.Sources)
	}
	if details.Sources[1].URL != "https://example.org/tui" {
		t.Fatalf("fragment was not removed: %#v", details.Sources[1])
	}
	if !strings.Contains(content, "Untrusted web search data") ||
		!strings.Contains(content, "bounded runtime design") {
		t.Fatalf("search content = %q", content)
	}
}

func TestResolveSearchURLUnwrapsBingRedirect(t *testing.T) {
	target := "https://go.dev/doc/"
	encoded := "a1" + base64.RawURLEncoding.EncodeToString([]byte(target))
	got := resolveSearchURL(mustURL(t, "https://www.bing.com/search"), "https://www.bing.com/ck/a?u="+encoded)
	if got != target {
		t.Fatalf("resolved URL = %q, want %q", got, target)
	}
}

func mustURL(t *testing.T, raw string) *url.URL {
	t.Helper()
	parsed, err := url.Parse(raw)
	if err != nil {
		t.Fatalf("parse URL: %v", err)
	}
	return parsed
}

func TestClientFetchExtractsHTMLAndTruncates(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(
			w,
			`<html><head><title>Ion docs</title><script>ignore()</script></head><body><h1>Ion</h1><p>Readable content.</p><style>hidden</style></body></html>`,
		)
	}))
	defer server.Close()

	client := testClient(server.URL)
	content, details, err := client.fetch(context.Background(), fetchRequest{URL: server.URL, MaxChars: 8})
	if err != nil {
		t.Fatalf("fetch error = %v", err)
	}
	if details.Status != http.StatusOK || details.FinalURL != server.URL || !details.Truncated {
		t.Fatalf("details = %#v", details)
	}
	if !strings.Contains(content, "Title: Ion docs") || strings.Contains(content, "ignore()") ||
		strings.Contains(content, "hidden") || !strings.Contains(content, "truncated") {
		t.Fatalf("fetch content = %q", content)
	}
}

func TestClientRejectsPrivateURLByDefault(t *testing.T) {
	client := NewClient(Config{ResolveIP: func(string) ([]net.IP, error) { return nil, nil }})
	_, _, err := client.fetch(context.Background(), fetchRequest{URL: "http://127.0.0.1:8080"})
	if !errors.Is(err, ErrPrivateURL) {
		t.Fatalf("error = %v, want private URL error", err)
	}
}

func TestClientRetriesTransientResponse(t *testing.T) {
	var requests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if requests.Add(1) == 1 {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		fmt.Fprint(w, `<a class="result__a" href="https://example.com">result</a>`)
	}))
	defer server.Close()

	client := testClient(server.URL)
	_, details, err := client.search(context.Background(), searchRequest{Query: "retry"})
	if err != nil {
		t.Fatalf("search error = %v", err)
	}
	if requests.Load() != 2 || len(details.Sources) != 1 {
		t.Fatalf("requests=%d details=%#v", requests.Load(), details)
	}
}

func TestClientCancellationStopsRetryWait(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer server.Close()

	client := testClient(server.URL)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, _, err := client.search(ctx, searchRequest{Query: "cancel"})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want cancellation", err)
	}
}

func TestLiveDefaultSearch(t *testing.T) {
	if os.Getenv("ION_WEB_LIVE") != "1" {
		t.Skip("set ION_WEB_LIVE=1 to run the authorized live web smoke")
	}
	client := NewClient(Config{Timeout: 20 * time.Second})
	content, details, err := client.search(context.Background(), searchRequest{Query: "Go programming language"})
	if err != nil {
		t.Fatalf("live search error = %v", err)
	}
	if len(details.Sources) == 0 || !strings.Contains(content, "Untrusted web search data") {
		t.Fatalf("live search returned no usable sources: details=%#v content=%q", details, content)
	}
}

func TestLiveDefaultFetch(t *testing.T) {
	if os.Getenv("ION_WEB_LIVE") != "1" {
		t.Skip("set ION_WEB_LIVE=1 to run the authorized live web smoke")
	}
	client := NewClient(Config{Timeout: 20 * time.Second})
	content, details, err := client.fetch(context.Background(), fetchRequest{URL: "https://go.dev/", MaxChars: 4000})
	if err != nil {
		t.Fatalf("live fetch error = %v", err)
	}
	if details.Status != http.StatusOK || !strings.Contains(content, "Untrusted web content") {
		t.Fatalf("live fetch returned unusable response: details=%#v content=%q", details, content)
	}
}
