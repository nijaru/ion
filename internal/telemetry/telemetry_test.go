package telemetry

import (
	"testing"

	"github.com/nijaru/ion/internal/config"
)

func TestNormalizeOTLPEndpoint(t *testing.T) {
	cases := []struct {
		name         string
		input        string
		wantEndpoint string
		wantInsecure bool
	}{
		{name: "host port", input: "localhost:4317", wantEndpoint: "localhost:4317"},
		{name: "https url", input: "https://otel.example.com:4317", wantEndpoint: "otel.example.com:4317"},
		{name: "http url", input: "http://localhost:4317", wantEndpoint: "localhost:4317", wantInsecure: true},
		{name: "empty", input: " ", wantEndpoint: ""},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			gotEndpoint, gotInsecure := normalizeOTLPEndpoint(tc.input)
			if gotEndpoint != tc.wantEndpoint {
				t.Fatalf("endpoint = %q, want %q", gotEndpoint, tc.wantEndpoint)
			}
			if gotInsecure != tc.wantInsecure {
				t.Fatalf("insecure = %v, want %v", gotInsecure, tc.wantInsecure)
			}
		})
	}
}

func TestSetupNoopWithoutEndpoint(t *testing.T) {
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "")
	t.Setenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "")
	t.Setenv("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", "")

	shutdown, err := Setup(t.Context(), &config.Config{})
	if err != nil {
		t.Fatalf("Setup returned error: %v", err)
	}
	if shutdown == nil {
		t.Fatal("Setup returned nil shutdown")
	}
	if err := shutdown(t.Context()); err != nil {
		t.Fatalf("shutdown returned error: %v", err)
	}
}
