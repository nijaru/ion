# Subagents

Subagents are not implemented in the current Ion runtime. There is no
`subagent` tool, persona loader, subagent configuration key, async child
manager, or child-event protocol exposed to users.

Ion intentionally keeps only the architectural seam needed for a future
Ion-owned implementation: session events carry a `SessionOrigin`, and the
session domain can represent subagent roles and links without making them
part of the normal transcript flow. Those types are not a supported runtime
capability today.

When subagents are promoted, the contract must define child lifecycle,
cancellation, failure propagation, context transfer, persistence, and TUI
projection together. A synchronous child tool may be a first slice; async
children and coordination views should follow only after that lifecycle is
owned and behaviorally tested. No Pi file format, argument, or protocol
compatibility is required.
