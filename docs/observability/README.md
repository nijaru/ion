# Observability

Ion can export Canto's OpenTelemetry traces and token/cost metrics to any OTLP
collector.

## Config

Use standard OpenTelemetry environment variables:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=localhost:4317 ion
```

Or configure Ion directly in `~/.ion/config.toml`:

```toml
telemetry_otlp_endpoint = "localhost:4317"
telemetry_otlp_insecure = true

[telemetry_otlp_headers]
x-honeycomb-team = "..."
```

Prompt and completion bodies are not recorded by default.

## Dashboard

`grafana/ion-overview.json` is a starter dashboard for OTLP metrics exported to
Prometheus by the OpenTelemetry Collector. Metric names assume the standard
Prometheus translation of Canto's `gen_ai.usage.*` instruments.
