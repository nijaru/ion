package main

import (
	"testing"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

func TestStartupMode(t *testing.T) {
	cases := []struct {
		name     string
		cfg      *config.Config
		modeFlag string
		yoloFlag bool
		want     session.Mode
		wantErr  bool
	}{
		{
			name: "defaults to trusted auto during stabilization",
			cfg:  &config.Config{},
			want: session.ModeYolo,
		},
		{
			name: "ignores config default during stabilization",
			cfg:  &config.Config{DefaultMode: "read"},
			want: session.ModeYolo,
		},
		{
			name:     "mode flag overrides config",
			cfg:      &config.Config{DefaultMode: "read"},
			modeFlag: "edit",
			want:     session.ModeEdit,
		},
		{
			name:     "yolo flag aliases auto mode",
			cfg:      &config.Config{DefaultMode: "read"},
			yoloFlag: true,
			want:     session.ModeYolo,
		},
		{
			name:     "yolo agrees with auto mode flag",
			modeFlag: "auto",
			yoloFlag: true,
			want:     session.ModeYolo,
		},
		{
			name:     "invalid mode flag",
			modeFlag: "full-send",
			wantErr:  true,
		},
		{
			name:     "conflicting yolo alias",
			modeFlag: "read",
			yoloFlag: true,
			wantErr:  true,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := startupMode(tc.cfg, tc.modeFlag, tc.yoloFlag)
			if tc.wantErr {
				if err == nil {
					t.Fatal("startupMode returned nil error")
				}
				return
			}
			if err != nil {
				t.Fatalf("startupMode returned error: %v", err)
			}
			if got != tc.want {
				t.Fatalf("startupMode = %v, want %v", got, tc.want)
			}
		})
	}
}

func TestApplyWorkspaceTrustModeGate(t *testing.T) {
	cases := []struct {
		name    string
		mode    session.Mode
		trusted bool
		want    session.Mode
	}{
		{
			name: "trusted keeps auto",
			mode: session.ModeYolo, trusted: true,
			want: session.ModeYolo,
		},
		{
			name: "untrusted interactive auto remains auto",
			mode: session.ModeYolo,
			want: session.ModeYolo,
		},
		{
			name: "untrusted edit remains edit",
			mode: session.ModeEdit,
			want: session.ModeEdit,
		},
		{
			name: "untrusted auto remains auto",
			mode: session.ModeYolo,
			want: session.ModeYolo,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := applyWorkspaceTrustModeGate(tc.mode, tc.trusted)
			if got != tc.want {
				t.Fatalf("mode = %v, want %v", got, tc.want)
			}
		})
	}
}
