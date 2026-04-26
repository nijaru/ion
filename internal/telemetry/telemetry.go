package telemetry

import (
	"context"
	"errors"
	"net/url"
	"os"
	"strings"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/exporters/otlp/otlpmetric/otlpmetricgrpc"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc"
	"go.opentelemetry.io/otel/sdk/metric"
	"go.opentelemetry.io/otel/sdk/resource"
	"go.opentelemetry.io/otel/sdk/trace"
	semconv "go.opentelemetry.io/otel/semconv/v1.37.0"

	"github.com/nijaru/ion/internal/config"
)

type ShutdownFunc func(context.Context) error

func Setup(ctx context.Context, cfg *config.Config) (ShutdownFunc, error) {
	endpoint := ""
	insecure := false
	headers := map[string]string(nil)
	if cfg != nil {
		endpoint = strings.TrimSpace(cfg.TelemetryOTLPEndpoint)
		insecure = cfg.TelemetryOTLPInsecure
		headers = cfg.TelemetryOTLPHeaders
	}
	if endpoint == "" && !hasOTLPEnv() {
		return func(context.Context) error { return nil }, nil
	}

	endpoint, endpointInsecure := normalizeOTLPEndpoint(endpoint)
	insecure = insecure || endpointInsecure

	res, err := resource.Merge(
		resource.Default(),
		resource.NewWithAttributes(
			semconv.SchemaURL,
			semconv.ServiceName("ion"),
		),
	)
	if err != nil {
		return nil, err
	}

	traceOpts := make([]otlptracegrpc.Option, 0, 3)
	metricOpts := make([]otlpmetricgrpc.Option, 0, 3)
	if endpoint != "" {
		traceOpts = append(traceOpts, otlptracegrpc.WithEndpoint(endpoint))
		metricOpts = append(metricOpts, otlpmetricgrpc.WithEndpoint(endpoint))
	}
	if insecure {
		traceOpts = append(traceOpts, otlptracegrpc.WithInsecure())
		metricOpts = append(metricOpts, otlpmetricgrpc.WithInsecure())
	}
	if len(headers) > 0 {
		traceOpts = append(traceOpts, otlptracegrpc.WithHeaders(headers))
		metricOpts = append(metricOpts, otlpmetricgrpc.WithHeaders(headers))
	}

	traceExporter, err := otlptracegrpc.New(ctx, traceOpts...)
	if err != nil {
		return nil, err
	}
	metricExporter, err := otlpmetricgrpc.New(ctx, metricOpts...)
	if err != nil {
		_ = traceExporter.Shutdown(ctx)
		return nil, err
	}

	tracerProvider := trace.NewTracerProvider(
		trace.WithBatcher(traceExporter),
		trace.WithResource(res),
	)
	meterProvider := metric.NewMeterProvider(
		metric.WithReader(metric.NewPeriodicReader(metricExporter)),
		metric.WithResource(res),
	)
	otel.SetTracerProvider(tracerProvider)
	otel.SetMeterProvider(meterProvider)

	return func(ctx context.Context) error {
		return errors.Join(
			meterProvider.Shutdown(ctx),
			tracerProvider.Shutdown(ctx),
		)
	}, nil
}

func hasOTLPEnv() bool {
	return os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT") != "" ||
		os.Getenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") != "" ||
		os.Getenv("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT") != ""
}

func normalizeOTLPEndpoint(endpoint string) (string, bool) {
	endpoint = strings.TrimSpace(endpoint)
	if endpoint == "" {
		return "", false
	}
	parsed, err := url.Parse(endpoint)
	if err != nil || parsed.Scheme == "" || parsed.Host == "" {
		return endpoint, false
	}
	insecure := parsed.Scheme == "http"
	return parsed.Host, insecure
}
