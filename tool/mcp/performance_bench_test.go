package mcp

import (
	"context"
	"os"
	"testing"
)

func BenchmarkRuntimeOpenClose(b *testing.B) {
	b.Setenv("ION_SANDBOX", "off")
	workdir := b.TempDir()
	configs := []ServerConfig{{
		Name:    "benchmark",
		Command: os.Args[0],
		Args:    []string{"-test.run=TestMCPHelperProcess"},
		Env:     map[string]string{"ION_MCP_HELPER": "1"},
	}}
	ctx := context.Background()
	b.ReportAllocs()
	for b.Loop() {
		runtime, err := Open(ctx, workdir, configs)
		if err != nil {
			b.Fatal(err)
		}
		if err := runtime.Close(); err != nil {
			b.Fatal(err)
		}
	}
}
