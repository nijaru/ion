package web

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/tool"
)

func TestToolsExposeBoundedNetworkContracts(t *testing.T) {
	client := NewClient(Config{AllowPrivateHosts: true})
	search := NewSearchTool(client)
	fetch := NewFetchTool(client)

	for _, tc := range []struct {
		name string
		tool interface {
			Spec() llm.Spec
			Metadata() tool.Metadata
			ApprovalRequirement(string) (tool.Requirement, bool, error)
		}
		args      string
		operation string
	}{
		{name: "search", tool: search, args: `{"query":"ion"}`, operation: "web_search"},
		{name: "fetch", tool: fetch, args: `{"url":"https://example.com"}`, operation: "web_fetch"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if tc.tool.Spec().Name == "" || tc.tool.Metadata().Category != "network" {
				t.Fatalf("spec/metadata = %#v/%#v", tc.tool.Spec(), tc.tool.Metadata())
			}
			requirement, required, err := tc.tool.ApprovalRequirement(tc.args)
			if err != nil || !required || requirement.Operation != tc.operation || requirement.NetworkIntent == "" {
				t.Fatalf("requirement=%#v required=%v err=%v", requirement, required, err)
			}
		})
	}
}

func TestFetchToolRejectsMalformedArguments(t *testing.T) {
	fetch := NewFetchTool(NewClient(Config{AllowPrivateHosts: true}))
	content, details, err := fetch.ExecuteDetailed(context.Background(), `{}`)
	zeroDetails, zeroType := details.(FetchDetails)
	if err == nil || content != "" || !zeroType || zeroDetails != (FetchDetails{}) ||
		!strings.Contains(err.Error(), "url is required") {
		t.Fatalf("content=%q details=%#v err=%v", content, details, err)
	}
}
