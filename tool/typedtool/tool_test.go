package typedtool

import (
	"context"
	"strings"
	"testing"

	basetool "github.com/nijaru/ion/tool"
)

type typedWeatherArgs struct {
	City string `json:"city" jsonschema:"required"`
}

type typedWeatherResult struct {
	Forecast string `json:"forecast"`
}

func TestNewExecutesTypedHandler(t *testing.T) {
	weather, err := New(Config[typedWeatherArgs, typedWeatherResult]{
		Name:        "weather",
		Description: "Get weather.",
		Metadata: basetool.Metadata{
			Category: "service",
			ReadOnly: true,
		},
		Execute: func(_ context.Context, args typedWeatherArgs) (typedWeatherResult, error) {
			if args.City != "Paris" {
				t.Fatalf("city = %q, want Paris", args.City)
			}
			return typedWeatherResult{Forecast: "clear"}, nil
		},
		Approval: func(args typedWeatherArgs) (basetool.Requirement, bool, error) {
			return basetool.Requirement{
				Category:  "service",
				Operation: "lookup",
				Resource:  args.City,
			}, true, nil
		},
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	if weather.Spec().Name != "weather" {
		t.Fatalf("spec name = %q, want weather", weather.Spec().Name)
	}
	if weather.Spec().Parameters == nil {
		t.Fatal("schema was not inferred")
	}
	if got := basetool.MetadataFor(weather); got.Category != "service" || !got.ReadOnly {
		t.Fatalf("metadata = %#v", got)
	}

	out, err := weather.Execute(t.Context(), `{"city":"Paris"}`)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(out, `"forecast":"clear"`) {
		t.Fatalf("output = %q, want JSON forecast", out)
	}

	req, ok, err := weather.ApprovalRequirement(`{"city":"Paris"}`)
	if err != nil {
		t.Fatalf("ApprovalRequirement: %v", err)
	}
	if !ok || req.Resource != "Paris" {
		t.Fatalf("approval = %#v, %v", req, ok)
	}
}

func TestRegisterRequiresRegistry(t *testing.T) {
	_, err := Register[typedWeatherArgs, typedWeatherResult](
		nil,
		Config[typedWeatherArgs, typedWeatherResult]{
			Name:        "weather",
			Description: "Get weather.",
			Execute: func(context.Context, typedWeatherArgs) (typedWeatherResult, error) {
				return typedWeatherResult{}, nil
			},
		},
	)
	if err == nil || !strings.Contains(err.Error(), "registry is required") {
		t.Fatalf("Register error = %v", err)
	}
}
