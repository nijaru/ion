# Subagents

Ion provides one built-in `subagent` tool for bounded delegation. The runtime
creates an isolated child session, snapshots the parent's model, prompt,
tools, policy, and workspace, and enforces child duration, output, token,
iteration, concurrency, and recursion limits. The child cannot delegate
again.

The parent owns the child lifecycle. Parent cancellation cancels the child;
provider errors and cleanup failures become visible child outcomes; the final
child result and budget are stored in the parent's durable tool result. Child
assistant/tool progress is projected through the ordinary tool-update stream,
so the CLI and TUI show live delegated work without owning a second transcript.

Child external-effect tools inherit the parent's action journal and policy.
Non-interactive child runs fail closed when confirmation is required. Trusted
local runs retain the launching user's ordinary permissions. The private child
transcript is not a second persistent conversation branch; the durable parent
result and action records are the recovery evidence for this bounded release.

Recursive delegation, remote execution, unbounded swarms, persona loaders,
and a general extension/package ecosystem remain out of scope. No Pi file
format, argument, or protocol compatibility is required.
