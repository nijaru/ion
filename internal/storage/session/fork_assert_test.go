package session

import "testing"

func assertChildForkPointMatchesLastOrigin(
	t *testing.T,
	child *Session,
	ancestry SessionAncestry,
) {
	t.Helper()

	events := child.Events()
	if len(events) == 0 {
		if ancestry.ForkPointEventID != "" {
			t.Fatalf(
				"empty child fork_point_event_id = %q, want empty",
				ancestry.ForkPointEventID,
			)
		}
		return
	}

	origin, ok, err := events[len(events)-1].ForkOrigin()
	if err != nil {
		t.Fatalf("fork origin decode: %v", err)
	}
	if !ok {
		t.Fatalf("child event missing fork origin metadata: %#v", events[len(events)-1].Metadata)
	}
	if ancestry.ForkPointEventID != origin.EventID {
		t.Fatalf(
			"child fork_point_event_id = %q, want last copied origin %q",
			ancestry.ForkPointEventID,
			origin.EventID,
		)
	}
}
