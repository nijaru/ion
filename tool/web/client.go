// Package web provides Ion-owned, bounded web research tools.
package web

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"golang.org/x/net/html"
)

const (
	DefaultSearchURL  = "https://www.bing.com/search"
	DefaultUserAgent  = "ion-web-research/1"
	DefaultMaxResults = 5
	MaxResultsLimit   = 10
	DefaultMaxChars   = 12000
	MaxCharsLimit     = 20000
	DefaultMaxBytes   = 1 << 20
	MaxURLLength      = 2048
	MaxRedirects      = 4
	maxAttempts       = 2
)

var (
	ErrInvalidURL     = errors.New("web URL is invalid")
	ErrPrivateURL     = errors.New("web URL targets a private or local address")
	ErrResponseTooBig = errors.New("web response exceeds the byte limit")
	// ErrUnsafeHTTPClient means a custom transport cannot be wrapped with the
	// private-host policy that protects the built-in web tools.
	ErrUnsafeHTTPClient = errors.New("web client transport cannot enforce private-host policy")
)

// Config controls one web client. AllowPrivateHosts is for deterministic
// local fixtures and must not be enabled by the production composition root.
type Config struct {
	HTTPClient        *http.Client
	SearchURL         string
	UserAgent         string
	MaxResults        int
	MaxChars          int
	MaxBytes          int64
	Timeout           time.Duration
	AllowPrivateHosts bool
	ResolveIP         func(string) ([]net.IP, error)
	Now               func() time.Time
}

// Client owns HTTP behavior shared by web_search and web_fetch.
type Client struct {
	httpClient        *http.Client
	searchURL         string
	userAgent         string
	maxResults        int
	maxChars          int
	maxBytes          int64
	allowPrivateHosts bool
	resolveIP         func(string) ([]net.IP, error)
	now               func() time.Time
	configurationErr  error
}

func NewClient(cfg Config) *Client {
	httpClient := &http.Client{}
	if cfg.HTTPClient != nil {
		copy := *cfg.HTTPClient
		httpClient = &copy
	}
	if cfg.Timeout > 0 {
		httpClient.Timeout = cfg.Timeout
	} else if httpClient.Timeout == 0 {
		httpClient.Timeout = 20 * time.Second
	}

	c := &Client{
		httpClient:        httpClient,
		searchURL:         strings.TrimSpace(cfg.SearchURL),
		userAgent:         strings.TrimSpace(cfg.UserAgent),
		maxResults:        cfg.MaxResults,
		maxChars:          cfg.MaxChars,
		maxBytes:          cfg.MaxBytes,
		allowPrivateHosts: cfg.AllowPrivateHosts,
		resolveIP:         cfg.ResolveIP,
		now:               cfg.Now,
	}
	if c.searchURL == "" {
		c.searchURL = DefaultSearchURL
	}
	if c.userAgent == "" {
		c.userAgent = DefaultUserAgent
	}
	if c.maxResults <= 0 || c.maxResults > MaxResultsLimit {
		c.maxResults = DefaultMaxResults
	}
	if c.maxChars <= 0 || c.maxChars > MaxCharsLimit {
		c.maxChars = DefaultMaxChars
	}
	if c.maxBytes <= 0 || c.maxBytes > DefaultMaxBytes {
		c.maxBytes = DefaultMaxBytes
	}
	if c.resolveIP == nil {
		c.resolveIP = net.LookupIP
	}
	if c.now == nil {
		c.now = time.Now
	}
	if !c.allowPrivateHosts {
		c.configurationErr = c.installPrivateHostPolicy()
	}

	previousRedirect := c.httpClient.CheckRedirect
	c.httpClient.CheckRedirect = func(req *http.Request, via []*http.Request) error {
		if len(via) >= MaxRedirects {
			return fmt.Errorf("web redirect limit exceeded (%d): %w", MaxRedirects, ErrInvalidURL)
		}
		if _, err := c.validateURL(req.URL.String()); err != nil {
			return fmt.Errorf("follow web redirect: %w", err)
		}
		if previousRedirect != nil {
			return previousRedirect(req, via)
		}
		return nil
	}
	return c
}

// privateHostTransport resolves each request once and dials one of the
// approved addresses from that same resolution. The standard transport would
// resolve a hostname again inside net.Dialer, leaving a DNS-rebinding window
// between URL validation and the actual connection.
type privateHostTransport struct {
	base       *http.Transport
	approvedIP func(*url.URL) ([]net.IP, error)
	dial       func(context.Context, string, string) (net.Conn, error)
}

type approvedDestinationKey struct{}

type approvedDestination struct {
	ips []net.IP
}

func (c *Client) installPrivateHostPolicy() error {
	base := c.httpClient.Transport
	if base == nil {
		base = http.DefaultTransport
	}
	transport, ok := base.(*http.Transport)
	if !ok {
		return fmt.Errorf("%w: got %T, want *http.Transport", ErrUnsafeHTTPClient, base)
	}
	clone := transport.Clone()
	// The built-in web policy is direct-address policy. A proxy would move the
	// network connection outside this transport's approved-address boundary.
	clone.Proxy = nil
	//lint:ignore SA1019 Clear the legacy hook so it cannot bypass DialContext.
	clone.Dial = nil
	//lint:ignore SA1019 Clear the legacy TLS hook so HTTPS also uses DialContext.
	clone.DialTLS = nil
	clone.DialTLSContext = nil
	policy := &privateHostTransport{
		base:       clone,
		approvedIP: c.approvedIPsForURL,
		dial:       (&net.Dialer{}).DialContext,
	}
	clone.DialContext = policy.dialContext
	c.httpClient.Transport = policy
	return nil
}

func (t *privateHostTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if req == nil || req.URL == nil {
		return nil, ErrInvalidURL
	}
	ips, err := t.approvedIP(req.URL)
	if err != nil {
		return nil, err
	}
	ctx := context.WithValue(req.Context(), approvedDestinationKey{}, approvedDestination{ips: cloneIPs(ips)})
	return t.base.RoundTrip(req.WithContext(ctx))
}

func (t *privateHostTransport) dialContext(ctx context.Context, network, address string) (net.Conn, error) {
	destination, ok := ctx.Value(approvedDestinationKey{}).(approvedDestination)
	if !ok || len(destination.ips) == 0 {
		return nil, fmt.Errorf("%w: approved destination is missing", ErrInvalidURL)
	}
	_, port, err := net.SplitHostPort(address)
	if err != nil {
		return nil, fmt.Errorf("%w: invalid dial address %q: %v", ErrInvalidURL, address, err)
	}
	var lastErr error
	for _, ip := range destination.ips {
		if network == "tcp4" && ip.To4() == nil {
			continue
		}
		if network == "tcp6" && ip.To4() != nil {
			continue
		}
		conn, dialErr := t.dial(ctx, network, net.JoinHostPort(ip.String(), port))
		if dialErr == nil {
			return conn, nil
		}
		lastErr = dialErr
	}
	if lastErr == nil {
		return nil, fmt.Errorf("no approved address supports network %q", network)
	}
	return nil, fmt.Errorf("dial approved web address for %q: %w", address, lastErr)
}

func cloneIPs(ips []net.IP) []net.IP {
	cloned := make([]net.IP, 0, len(ips))
	for _, ip := range ips {
		cloned = append(cloned, append(net.IP(nil), ip...))
	}
	return cloned
}

type searchRequest struct {
	Query      string
	MaxResults int
}

type fetchRequest struct {
	URL      string
	MaxChars int
}

type Source struct {
	Title   string `json:"title"`
	URL     string `json:"url"`
	Snippet string `json:"snippet,omitempty"`
}

type SearchDetails struct {
	Query       string    `json:"query"`
	Sources     []Source  `json:"sources"`
	RetrievedAt time.Time `json:"retrieved_at"`
	Provider    string    `json:"provider"`
	Truncated   bool      `json:"truncated"`
	ResultLimit int       `json:"result_limit"`
}

type FetchDetails struct {
	URL         string    `json:"url"`
	FinalURL    string    `json:"final_url"`
	Status      int       `json:"status"`
	ContentType string    `json:"content_type,omitempty"`
	RetrievedAt time.Time `json:"retrieved_at"`
	Bytes       int       `json:"bytes"`
	Chars       int       `json:"chars"`
	Truncated   bool      `json:"truncated"`
}

func (c *Client) search(ctx context.Context, req searchRequest) (string, SearchDetails, error) {
	query := strings.TrimSpace(req.Query)
	if query == "" {
		return "", SearchDetails{}, fmt.Errorf("web search query is required")
	}
	if len(query) > 400 {
		return "", SearchDetails{}, fmt.Errorf("web search query exceeds 400 characters")
	}
	limit := req.MaxResults
	if limit <= 0 {
		limit = c.maxResults
	}
	if limit > MaxResultsLimit {
		return "", SearchDetails{}, fmt.Errorf("web search max_results exceeds %d", MaxResultsLimit)
	}

	retrievedAt := time.Now().UTC()
	if c.now != nil {
		retrievedAt = c.now()
	}

	if c.searchURL == DefaultSearchURL {
		if apiKey := os.Getenv("EXA_API_KEY"); apiKey != "" {
			if sources, exaErr := c.searchExa(ctx, apiKey, query, limit); exaErr == nil && len(sources) > 0 {
				return c.formatSearchResults(query, sources, "exa.ai", limit, retrievedAt)
			}
		}
		if apiKey := os.Getenv("BRAVE_API_KEY"); apiKey != "" {
			if sources, braveErr := c.searchBrave(ctx, apiKey, query, limit); braveErr == nil && len(sources) > 0 {
				return c.formatSearchResults(query, sources, "search.brave.com", limit, retrievedAt)
			}
		}
	}

	endpoint, err := c.validateURL(c.searchURL)
	if err != nil {
		return "", SearchDetails{}, fmt.Errorf("search endpoint: %w", err)
	}
	values := endpoint.Query()
	values.Set("q", query)
	endpoint.RawQuery = values.Encode()
	response, body, retrievedAtResp, err := c.get(ctx, endpoint.String())
	if err != nil {
		return "", SearchDetails{}, fmt.Errorf("web search: %w", err)
	}
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return "", SearchDetails{}, httpStatusError("web search", response.StatusCode)
	}
	sources, _, err := parseSearchResults(endpoint, body, limit, c)
	if err != nil {
		return "", SearchDetails{}, fmt.Errorf("parse web search results: %w", err)
	}
	return c.formatSearchResults(query, sources, endpoint.Hostname(), limit, retrievedAtResp)
}

func (c *Client) searchExa(ctx context.Context, apiKey string, query string, limit int) ([]Source, error) {
	reqBody, err := json.Marshal(map[string]any{
		"query":         query,
		"numResults":    limit,
		"useAutoprompt": true,
	})
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, "https://api.exa.ai/search", bytes.NewReader(reqBody))
	if err != nil {
		return nil, err
	}
	req.Header.Set("x-api-key", apiKey)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("User-Agent", c.userAgent)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, httpStatusError("exa search", resp.StatusCode)
	}

	var data struct {
		Results []struct {
			Title string `json:"title"`
			URL   string `json:"url"`
			Text  string `json:"text"`
		} `json:"results"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&data); err != nil {
		return nil, err
	}
	sources := make([]Source, 0, len(data.Results))
	for _, item := range data.Results {
		if item.URL != "" && item.Title != "" {
			snippet := strings.TrimSpace(item.Text)
			if len([]rune(snippet)) > 300 {
				snippet = string([]rune(snippet)[:300]) + "..."
			}
			sources = append(sources, Source{
				Title:   item.Title,
				URL:     item.URL,
				Snippet: snippet,
			})
		}
	}
	return sources, nil
}

func (c *Client) searchBrave(ctx context.Context, apiKey string, query string, limit int) ([]Source, error) {
	reqURL := fmt.Sprintf("https://api.search.brave.com/res/v1/web/search?q=%s&count=%d", url.QueryEscape(query), limit)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, reqURL, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("X-Subscription-Token", apiKey)
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", c.userAgent)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, httpStatusError("brave search", resp.StatusCode)
	}

	var data struct {
		Web struct {
			Results []struct {
				Title       string `json:"title"`
				URL         string `json:"url"`
				Description string `json:"description"`
			} `json:"results"`
		} `json:"web"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&data); err != nil {
		return nil, err
	}
	sources := make([]Source, 0, len(data.Web.Results))
	for _, item := range data.Web.Results {
		if item.URL != "" && item.Title != "" {
			sources = append(sources, Source{
				Title:   item.Title,
				URL:     item.URL,
				Snippet: item.Description,
			})
		}
	}
	return sources, nil
}

func (c *Client) formatSearchResults(
	query string,
	sources []Source,
	provider string,
	limit int,
	retrievedAt time.Time,
) (string, SearchDetails, error) {
	truncated := len(sources) > limit
	if truncated {
		sources = sources[:limit]
	}
	details := SearchDetails{
		Query:       query,
		Sources:     sources,
		RetrievedAt: retrievedAt,
		Provider:    provider,
		Truncated:   truncated,
		ResultLimit: limit,
	}
	var out strings.Builder
	fmt.Fprintf(&out, "Untrusted web search data for %q (retrieved %s):\n\n", query, retrievedAt.Format(time.RFC3339))
	if len(sources) == 0 {
		out.WriteString("No bounded results were returned.")
	} else {
		for i, source := range sources {
			fmt.Fprintf(&out, "%d. %s\n%s\n", i+1, source.Title, source.URL)
			if source.Snippet != "" {
				fmt.Fprintf(&out, "%s\n", source.Snippet)
			}
			if i+1 < len(sources) {
				out.WriteByte('\n')
			}
		}
	}
	if truncated {
		out.WriteString("\n[search results truncated by result limit]")
	}
	return out.String(), details, nil
}

func (c *Client) fetch(ctx context.Context, req fetchRequest) (string, FetchDetails, error) {
	inputURL := strings.TrimSpace(req.URL)
	if inputURL == "" {
		return "", FetchDetails{}, fmt.Errorf("web fetch url is required")
	}
	validated, err := c.validateURL(inputURL)
	if err != nil {
		return "", FetchDetails{}, fmt.Errorf("web fetch: %w", err)
	}
	maxChars := req.MaxChars
	if maxChars <= 0 {
		maxChars = c.maxChars
	}
	if maxChars > MaxCharsLimit {
		return "", FetchDetails{}, fmt.Errorf("web fetch max_chars exceeds %d", MaxCharsLimit)
	}
	response, body, retrievedAt, err := c.get(ctx, validated.String())
	if err != nil {
		return "", FetchDetails{}, fmt.Errorf("web fetch: %w", err)
	}
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return "", FetchDetails{}, httpStatusError("web fetch", response.StatusCode)
	}
	text, title := extractText(response.Header.Get("Content-Type"), body)
	text = strings.TrimSpace(text)
	truncated := false
	if len([]rune(text)) > maxChars {
		text = string([]rune(text)[:maxChars]) + "\n[web content truncated by character limit]"
		truncated = true
	}
	details := FetchDetails{
		URL:         validated.String(),
		FinalURL:    response.Request.URL.String(),
		Status:      response.StatusCode,
		ContentType: response.Header.Get("Content-Type"),
		RetrievedAt: retrievedAt,
		Bytes:       len(body),
		Chars:       len([]rune(text)),
		Truncated:   truncated,
	}
	var out strings.Builder
	fmt.Fprintf(&out, "Untrusted web content from %s", response.Request.URL.String())
	if title != "" {
		fmt.Fprintf(&out, "\nTitle: %s", title)
	}
	out.WriteString("\n\n")
	out.WriteString(text)
	if text == "" {
		out.WriteString("[no readable text]")
	}
	return out.String(), details, nil
}

func (c *Client) get(ctx context.Context, rawURL string) (*http.Response, []byte, time.Time, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	if c.configurationErr != nil {
		return nil, nil, time.Time{}, c.configurationErr
	}
	for attempt := 0; attempt < maxAttempts; attempt++ {
		if err := ctx.Err(); err != nil {
			return nil, nil, time.Time{}, err
		}
		request, err := http.NewRequestWithContext(ctx, http.MethodGet, rawURL, nil)
		if err != nil {
			return nil, nil, time.Time{}, err
		}
		request.Header.Set("Accept", "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8")
		request.Header.Set("User-Agent", c.userAgent)
		response, err := c.httpClient.Do(request)
		if err != nil {
			if ctx.Err() != nil {
				return nil, nil, time.Time{}, ctx.Err()
			}
			if attempt+1 < maxAttempts {
				if err := waitRetry(ctx); err != nil {
					return nil, nil, time.Time{}, err
				}
				continue
			}
			return nil, nil, time.Time{}, err
		}
		body, readErr := io.ReadAll(io.LimitReader(response.Body, c.maxBytes+1))
		response.Body.Close()
		if readErr != nil {
			return nil, nil, time.Time{}, readErr
		}
		if int64(len(body)) > c.maxBytes {
			return nil, nil, time.Time{}, ErrResponseTooBig
		}
		if retryableStatus(response.StatusCode) && attempt+1 < maxAttempts {
			if err := waitRetry(ctx); err != nil {
				return nil, nil, time.Time{}, err
			}
			continue
		}
		return response, body, c.now(), nil
	}
	return nil, nil, time.Time{}, errors.New("web request attempts exhausted")
}

func waitRetry(ctx context.Context) error {
	timer := time.NewTimer(100 * time.Millisecond)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func retryableStatus(status int) bool {
	return status == http.StatusRequestTimeout || status == http.StatusTooManyRequests || status >= 500
}

func httpStatusError(operation string, status int) error {
	return fmt.Errorf("%s returned HTTP %d", operation, status)
}

func (c *Client) validateURL(raw string) (*url.URL, error) {
	if len(raw) > MaxURLLength {
		return nil, fmt.Errorf("%w: exceeds %d characters", ErrInvalidURL, MaxURLLength)
	}
	u, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || u.Scheme == "" || u.Hostname() == "" || u.User != nil {
		return nil, fmt.Errorf("%w: %q", ErrInvalidURL, raw)
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return nil, fmt.Errorf("%w: only http and https are supported", ErrInvalidURL)
	}
	if c.allowPrivateHosts {
		return u, nil
	}
	if _, err := c.approvedIPsForURL(u); err != nil {
		return nil, err
	}
	return u, nil
}

func (c *Client) approvedIPsForURL(u *url.URL) ([]net.IP, error) {
	if u == nil || len(u.String()) > MaxURLLength || u.Scheme == "" || u.Hostname() == "" || u.User != nil {
		return nil, ErrInvalidURL
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return nil, fmt.Errorf("%w: only http and https are supported", ErrInvalidURL)
	}
	host := strings.ToLower(strings.TrimSuffix(u.Hostname(), "."))
	if host == "localhost" || strings.HasSuffix(host, ".localhost") || strings.HasSuffix(host, ".local") {
		return nil, ErrPrivateURL
	}
	if ip := net.ParseIP(host); ip != nil {
		if privateIP(ip) {
			return nil, ErrPrivateURL
		}
		return []net.IP{ip}, nil
	}
	ips, err := c.resolveIP(host)
	if err != nil {
		return nil, fmt.Errorf("resolve %q: %w", host, err)
	}
	if len(ips) == 0 {
		return nil, fmt.Errorf("resolve %q: no addresses returned", host)
	}
	for _, ip := range ips {
		if ip == nil || ip.To16() == nil {
			return nil, fmt.Errorf("resolve %q: invalid address", host)
		}
		if privateIP(ip) {
			return nil, ErrPrivateURL
		}
	}
	return cloneIPs(ips), nil
}

func privateIP(ip net.IP) bool {
	return ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() ||
		ip.IsUnspecified() || ip.IsMulticast()
}

func extractText(contentType string, body []byte) (string, string) {
	if !strings.Contains(strings.ToLower(contentType), "html") {
		return string(body), ""
	}
	doc, err := html.Parse(strings.NewReader(string(body)))
	if err != nil {
		return string(body), ""
	}
	var text strings.Builder
	var title string
	var walk func(*html.Node, bool)
	walk = func(node *html.Node, skip bool) {
		if node == nil {
			return
		}
		if node.Type == html.ElementNode {
			tag := strings.ToLower(node.Data)
			switch tag {
			case "head", "script", "style", "noscript", "template", "svg", "iframe":
				skip = true
			case "title":
				title = strings.TrimSpace(nodeText(node))
				return
			case "h1":
				text.WriteString("\n\n# ")
			case "h2":
				text.WriteString("\n\n## ")
			case "h3":
				text.WriteString("\n\n### ")
			case "h4":
				text.WriteString("\n\n#### ")
			case "h5", "h6":
				text.WriteString("\n\n##### ")
			case "p", "div", "section", "article":
				text.WriteString("\n\n")
			case "br":
				text.WriteString("\n")
			case "li":
				text.WriteString("\n- ")
			case "pre":
				text.WriteString("\n\n```\n")
			case "code":
				if node.Parent == nil || strings.ToLower(node.Parent.Data) != "pre" {
					text.WriteString("`")
				}
			case "blockquote":
				text.WriteString("\n\n> ")
			}
		}
		if node.Type == html.TextNode && !skip {
			content := node.Data
			if node.Parent != nil && strings.ToLower(node.Parent.Data) == "pre" {
				text.WriteString(content)
			} else {
				value := strings.Join(strings.Fields(content), " ")
				if value != "" {
					if text.Len() > 0 {
						last := text.String()[text.Len()-1]
						if last != '\n' && last != ' ' && last != '#' && last != '>' && last != '`' {
							text.WriteByte(' ')
						}
					}
					text.WriteString(value)
				}
			}
		}
		for child := node.FirstChild; child != nil; child = child.NextSibling {
			walk(child, skip)
		}
		if node.Type == html.ElementNode && !skip {
			tag := strings.ToLower(node.Data)
			switch tag {
			case "h1", "h2", "h3", "h4", "h5", "h6", "p", "blockquote":
				text.WriteString("\n\n")
			case "pre":
				text.WriteString("\n```\n\n")
			case "code":
				if node.Parent == nil || strings.ToLower(node.Parent.Data) != "pre" {
					text.WriteString("`")
				}
			}
		}
	}
	walk(doc, false)
	raw := text.String()
	lines := strings.Split(raw, "\n")
	var cleaned []string
	blankCount := 0
	for _, l := range lines {
		trimmed := strings.TrimRight(l, " \t\r")
		if strings.TrimSpace(trimmed) == "" {
			blankCount++
			if blankCount <= 2 {
				cleaned = append(cleaned, "")
			}
		} else {
			blankCount = 0
			cleaned = append(cleaned, trimmed)
		}
	}
	return strings.TrimSpace(strings.Join(cleaned, "\n")), title
}

func nodeText(node *html.Node) string {
	var text strings.Builder
	var walk func(*html.Node)
	walk = func(current *html.Node) {
		if current.Type == html.TextNode {
			text.WriteString(current.Data)
		}
		for child := current.FirstChild; child != nil; child = child.NextSibling {
			walk(child)
		}
	}
	walk(node)
	return strings.Join(strings.Fields(text.String()), " ")
}

func parseSearchResults(endpoint *url.URL, body []byte, limit int, client *Client) ([]Source, bool, error) {
	doc, err := html.Parse(strings.NewReader(string(body)))
	if err != nil {
		return nil, false, err
	}
	results := make([]Source, 0, limit+1)
	seen := make(map[string]struct{}, limit+1)
	appendSource := func(source Source) {
		if len(results) > limit || source.URL == "" || source.Title == "" {
			return
		}
		if _, exists := seen[source.URL]; exists {
			return
		}
		if _, validationErr := client.validateURL(source.URL); validationErr != nil {
			return
		}
		seen[source.URL] = struct{}{}
		results = append(results, source)
	}
	var walk func(*html.Node)
	walk = func(node *html.Node) {
		if node == nil || len(results) > limit {
			return
		}
		if node.Type == html.ElementNode && hasClass(node, "b_algo") {
			if source, ok := parseBingResult(endpoint, node); ok {
				appendSource(source)
			}
			return
		}
		if node.Type == html.ElementNode && strings.EqualFold(node.Data, "a") && hasClass(node, "result__a") {
			if resolved := resolveSearchURL(endpoint, attribute(node, "href")); resolved != "" {
				appendSource(Source{
					Title:   nodeText(node),
					URL:     resolved,
					Snippet: searchSnippet(node),
				})
			}
		}
		for child := node.FirstChild; child != nil; child = child.NextSibling {
			walk(child)
		}
	}
	walk(doc)
	truncated := len(results) > limit
	if truncated {
		results = results[:limit]
	}
	return results, truncated, nil
}

func parseBingResult(endpoint *url.URL, node *html.Node) (Source, bool) {
	heading := findElement(node, func(candidate *html.Node) bool {
		return candidate.Type == html.ElementNode && strings.EqualFold(candidate.Data, "h2")
	})
	if heading == nil {
		return Source{}, false
	}
	anchor := findElement(heading, func(candidate *html.Node) bool {
		return candidate.Type == html.ElementNode && strings.EqualFold(candidate.Data, "a")
	})
	if anchor == nil {
		return Source{}, false
	}
	resolved := resolveSearchURL(endpoint, attribute(anchor, "href"))
	if resolved == "" {
		return Source{}, false
	}
	snippet := findClassText(node, "b_caption")
	if snippet == "" {
		snippet = findTagText(node, "p")
	}
	return Source{Title: nodeText(anchor), URL: resolved, Snippet: snippet}, true
}

func findElement(node *html.Node, predicate func(*html.Node) bool) *html.Node {
	if predicate(node) {
		return node
	}
	for child := node.FirstChild; child != nil; child = child.NextSibling {
		if found := findElement(child, predicate); found != nil {
			return found
		}
	}
	return nil
}

func findTagText(node *html.Node, tag string) string {
	if node.Type == html.ElementNode && strings.EqualFold(node.Data, tag) {
		return nodeText(node)
	}
	for child := node.FirstChild; child != nil; child = child.NextSibling {
		if result := findTagText(child, tag); result != "" {
			return result
		}
	}
	return ""
}

func resolveSearchURL(endpoint *url.URL, raw string) string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return ""
	}
	if strings.HasPrefix(raw, "//") {
		raw = endpoint.Scheme + ":" + raw
	}
	parsed, err := url.Parse(raw)
	if err != nil {
		return ""
	}
	if parsed.Hostname() == "duckduckgo.com" || strings.HasSuffix(parsed.Hostname(), ".duckduckgo.com") {
		if target := parsed.Query().Get("uddg"); target != "" {
			decoded, err := url.QueryUnescape(target)
			if err == nil {
				raw = decoded
				parsed, err = url.Parse(raw)
				if err != nil {
					return ""
				}
			}
		}
	}
	if strings.HasSuffix(parsed.Hostname(), ".bing.com") || parsed.Hostname() == "bing.com" {
		if encoded := parsed.Query().Get("u"); strings.HasPrefix(encoded, "a1") {
			if decoded, ok := decodeBingTarget(strings.TrimPrefix(encoded, "a1")); ok {
				raw = decoded
				parsed, err = url.Parse(raw)
				if err != nil {
					return ""
				}
			}
		}
	}
	if !parsed.IsAbs() {
		parsed = endpoint.ResolveReference(parsed)
	}
	parsed.Fragment = ""
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return ""
	}
	return parsed.String()
}

func decodeBingTarget(encoded string) (string, bool) {
	for _, encoding := range []*base64.Encoding{base64.RawURLEncoding, base64.URLEncoding} {
		decoded, err := encoding.DecodeString(encoded)
		if err == nil {
			candidate := string(decoded)
			parsed, parseErr := url.Parse(candidate)
			if parseErr == nil && (parsed.Scheme == "http" || parsed.Scheme == "https") {
				return candidate, true
			}
		}
	}
	return "", false
}

func searchSnippet(node *html.Node) string {
	for parent := node.Parent; parent != nil; parent = parent.Parent {
		if hasClass(parent, "result") {
			if snippet := findClassText(parent, "result__snippet"); snippet != "" {
				return snippet
			}
			break
		}
	}
	return ""
}

func findClassText(node *html.Node, class string) string {
	if hasClass(node, class) {
		return nodeText(node)
	}
	for child := node.FirstChild; child != nil; child = child.NextSibling {
		if result := findClassText(child, class); result != "" {
			return result
		}
	}
	return ""
}

func hasClass(node *html.Node, class string) bool {
	return containsString(strings.Fields(attribute(node, "class")), class)
}

func containsString(values []string, target string) bool {
	for _, value := range values {
		if value == target {
			return true
		}
	}
	return false
}

func attribute(node *html.Node, name string) string {
	for _, attr := range node.Attr {
		if attr.Key == name {
			return attr.Val
		}
	}
	return ""
}
