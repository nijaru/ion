package session

import "testing"

func TestDecideSubmitPreflight(t *testing.T) {
	tests := []struct {
		name  string
		input SubmitPreflightInput
		want  SubmitPreflightDecision
	}{
		{
			name:  "runtime not required",
			input: SubmitPreflightInput{},
			want:  SubmitPreflightDecision{Allowed: true},
		},
		{
			name: "missing provider",
			input: SubmitPreflightInput{
				RuntimeRequired: true,
				Model:           "gpt-5.4",
			},
			want: SubmitPreflightDecision{Reason: NoProviderConfiguredStatus()},
		},
		{
			name: "missing model",
			input: SubmitPreflightInput{
				RuntimeRequired: true,
				Provider:        "openrouter",
			},
			want: SubmitPreflightDecision{Reason: NoModelConfiguredStatus()},
		},
		{
			name: "session budget already reached",
			input: SubmitPreflightInput{
				RuntimeRequired: true,
				Provider:        "openrouter",
				Model:           "gpt-5.4",
				TotalCost:       0.05,
				MaxSessionCost:  0.05,
			},
			want: SubmitPreflightDecision{
				Reason: "session cost limit reached ($0.050000 / $0.050000)",
			},
		},
		{
			name: "configured and within budget",
			input: SubmitPreflightInput{
				RuntimeRequired: true,
				Provider:        "openrouter",
				Model:           "gpt-5.4",
				TotalCost:       0.01,
				MaxSessionCost:  0.05,
			},
			want: SubmitPreflightDecision{Allowed: true},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideSubmitPreflight(tt.input); got != tt.want {
				t.Fatalf("DecideSubmitPreflight() = %#v, want %#v", got, tt.want)
			}
		})
	}
}
