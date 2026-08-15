package llm

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/ion/config"
)

// EndpointResolver owns local endpoint discovery for one host. Its cache and
// HTTP client are deliberately instance state so separate CLI/TUI hosts and
// tests cannot observe one another's probe results.
type EndpointResolver struct {
	mu     sync.RWMutex
	cache  map[string]localProbeResult
	client *http.Client
}

type localProbeResult struct {
	endpoint string
	ready    bool
	checked  time.Time
}

type EndpointResolverOptions struct {
	HTTPClient *http.Client
}

const (
	localProbeTTL     = 5 * time.Second
	localProbeTimeout = 300 * time.Millisecond
)

func NewEndpointResolver(opts EndpointResolverOptions) *EndpointResolver {
	client := opts.HTTPClient
	if client == nil {
		client = &http.Client{Transport: http.DefaultTransport}
	}
	return &EndpointResolver{
		cache:  make(map[string]localProbeResult),
		client: client,
	}
}

// Resolve returns the configured endpoint or a provider default. Local
// OpenAI-compatible providers are probed through this resolver's owned cache.
func (r *EndpointResolver) Resolve(ctx context.Context, cfg *config.Config) string {
	if r == nil || cfg == nil {
		return ""
	}
	if ctx == nil {
		ctx = context.Background()
	}
	def, ok := LookupConfig(cfg, cfg.Provider)
	if !ok {
		return ""
	}
	if cfg.Providers != nil {
		if ps, ok := cfg.Providers[cfg.Provider]; ok && strings.TrimSpace(ps.Endpoint) != "" {
			return strings.TrimSpace(ps.Endpoint)
		}
	}
	if endpoint := strings.TrimSpace(cfg.Endpoint); endpoint != "" && def.SupportsCustomEndpoint {
		return endpoint
	}
	if def.ID == OpenAICompatibleID || (def.Kind == KindCustom && def.Family == FamilyOpenAI) {
		if endpoint, ok := r.Probe(ctx, cfg); ok {
			return endpoint
		}
	}
	return strings.TrimSpace(def.DefaultEndpoint)
}

func (r *EndpointResolver) Probe(ctx context.Context, cfg *config.Config) (string, bool) {
	return r.probe(ctx, cfg, true)
}

func (r *EndpointResolver) ProbeFresh(ctx context.Context, cfg *config.Config) (string, bool) {
	return r.probe(ctx, cfg, false)
}

func (r *EndpointResolver) CachedState(cfg *config.Config) (endpoint string, ready bool, ok bool) {
	if r == nil {
		return "", false, false
	}
	for _, target := range localAPIProbeTargets(cfg) {
		cached, hit := r.cached(target)
		if hit {
			return cached.endpoint, cached.ready, true
		}
	}
	return "", false, false
}

func (r *EndpointResolver) probe(ctx context.Context, cfg *config.Config, useCache bool) (string, bool) {
	if r == nil {
		return "", false
	}
	if ctx == nil {
		ctx = context.Background()
	}
	for _, endpoint := range localAPIProbeTargets(cfg) {
		if useCache {
			if cached, ok := r.cached(endpoint); ok {
				if cached.ready {
					return cached.endpoint, true
				}
				continue
			}
		}
		ready := r.probeEndpoint(ctx, endpoint, cfg)
		r.store(endpoint, ready)
		if ready {
			return endpoint, true
		}
	}
	return "", false
}

func localAPIProbeTargets(cfg *config.Config) []string {
	seen := map[string]struct{}{}
	targets := make([]string, 0, 4)
	add := func(raw string) {
		value := strings.TrimRight(strings.TrimSpace(raw), "/")
		if value == "" {
			return
		}
		if _, ok := seen[value]; ok {
			return
		}
		seen[value] = struct{}{}
		targets = append(targets, value)
	}

	if cfg != nil && IsOpenAICompatible(cfg.Provider) {
		add(cfg.Endpoint)
		if strings.TrimSpace(cfg.Endpoint) != "" {
			return targets
		}
	}
	add("http://127.0.0.1:1234/v1")
	add("http://127.0.0.1:8000/v1")
	add("http://127.0.0.1:8080/v1")
	return targets
}

func (r *EndpointResolver) cached(endpoint string) (localProbeResult, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	result, ok := r.cache[endpoint]
	if !ok || time.Since(result.checked) > localProbeTTL {
		return localProbeResult{}, false
	}
	return result, true
}

func (r *EndpointResolver) store(endpoint string, ready bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.cache[endpoint] = localProbeResult{
		endpoint: endpoint,
		ready:    ready,
		checked:  time.Now(),
	}
}

func (r *EndpointResolver) probeEndpoint(ctx context.Context, endpoint string, cfg *config.Config) bool {
	reqCtx := ctx
	if _, ok := reqCtx.Deadline(); !ok {
		var cancel context.CancelFunc
		reqCtx, cancel = context.WithTimeout(reqCtx, localProbeTimeout)
		defer cancel()
	}
	req, err := http.NewRequestWithContext(
		reqCtx,
		http.MethodGet,
		strings.TrimRight(endpoint, "/")+"/models",
		nil,
	)
	if err != nil {
		return false
	}
	req.Header.Set("User-Agent", "ion/0.0.0")
	if cfg != nil {
		if def, ok := Lookup(cfg.Provider); ok {
			if token := ResolvedAuthToken(cfg, def); token != "" {
				req.Header.Set("Authorization", "Bearer "+token)
			}
			for key, value := range ResolvedHeaders(cfg) {
				req.Header.Set(key, value)
			}
		}
	}

	client := r.client
	if client == nil {
		return false
	}
	response, err := client.Do(req)
	if err != nil {
		return false
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return false
	}
	body, err := io.ReadAll(response.Body)
	if err != nil {
		return false
	}
	var payload struct {
		Data []struct {
			ID string `json:"id"`
		} `json:"data"`
	}
	return json.Unmarshal(body, &payload) == nil
}
