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
			name: "defaults to edit",
			cfg:  &config.Config{},
			want: session.ModeEdit,
		},
		{
			name: "uses config default",
			cfg:  &config.Config{DefaultMode: "read"},
			want: session.ModeRead,
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
