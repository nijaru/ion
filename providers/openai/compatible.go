package openai

import (
	"net/http"
	"os"

	"github.com/nijaru/ion/llm"
	sashaoai "github.com/sashabaranov/go-openai"
)

type CompatibleSpec struct {
	ID                 string
	DefaultAPIEndpoint string
	APIKeyEnvVars      []string
	DefaultHeaders     map[string]string
	ModelCaps          map[string]llm.Capabilities
	DefaultAPIKey      string
}

type headerTransport struct {
	http.RoundTripper
	headers map[string]string
}

func (t *headerTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	for k, v := range t.headers {
		req.Header.Set(k, v)
	}
	return t.RoundTripper.RoundTrip(req)
}

func NewCompatibleProvider(cfg llm.ProviderConfig, spec CompatibleSpec) *Provider {
	if cfg.ID == "" {
		cfg.ID = spec.ID
	}
	if cfg.APIEndpoint == "" {
		cfg.APIEndpoint = spec.DefaultAPIEndpoint
	}
	if len(cfg.DefaultHeaders) == 0 && len(spec.DefaultHeaders) > 0 {
		cfg.DefaultHeaders = cloneHeaders(spec.DefaultHeaders)
	}

	apiKey := resolveAPIKey(cfg.APIKey, spec.APIKeyEnvVars)
	if apiKey == "" {
		apiKey = spec.DefaultAPIKey
	}

	config := sashaoai.DefaultConfig(apiKey)
	if cfg.APIEndpoint != "" {
		config.BaseURL = cfg.APIEndpoint
	}
	if len(cfg.DefaultHeaders) > 0 {
		config.HTTPClient = &http.Client{
			Transport: &headerTransport{
				RoundTripper: http.DefaultTransport,
				headers:      cfg.DefaultHeaders,
			},
		}
	}

	return &Provider{
		Base: Base{
			Client:    sashaoai.NewClientWithConfig(config),
			Config:    cfg,
			ModelCaps: spec.ModelCaps,
		},
	}
}

func resolveAPIKey(apiKey string, envVars []string) string {
	value := apiKey
	for _, envVar := range envVars {
		if value == "" || value == "$"+envVar {
			if resolved := os.Getenv(envVar); resolved != "" {
				return resolved
			}
		}
	}
	return value
}

func cloneHeaders(src map[string]string) map[string]string {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]string, len(src))
	for k, v := range src {
		dst[k] = v
	}
	return dst
}
