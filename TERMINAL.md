# Ion terminal design

Status: proposed interaction contract, revision 1, 2026-09-11. This is the target for the redesigned TUI, not a claim that the current binary implements it. [DESIGN.md](DESIGN.md) owns runtime semantics; [ROADMAP.md](ROADMAP.md) owns delivery and acceptance. The preceding renderer contract is preserved under [history](docs/history/README.md).

## 1. Product model

The TUI is a coding workspace for one agent or a cooperating group. Ordinary use stays a conversation with a composer and status line. Enabling workers adds visibility and control without replacing the main conversation or requiring the user to operate an orchestration dashboard.

Every agent remains directly inspectable. A worker can be prompted or steered with the same interaction as the root, subject to its authority and state. All commands name stable runtime IDs; display names are labels, not routing authority.

The main views are conversation, agent group, focused worker, activity/approvals, and changes. They are views of the same session, not independent runtimes.

## 2. Conversation view

The proposed default is Pi-like inline conversation with useful native scrollback, a stable live band, a multiline composer, and compact session status. Fullscreen conversation is an equivalent navigation surface, not a separate feature set.

The composer displays its target agent, effective model, and delivery mode. It supports editing, undo, multiline paste, history, file references, completion, image attachment where supported, and an external editor. Unsupported terminal chords have documented alternatives. A paste never becomes a sequence of submissions.

Completed messages are immutable logical transcript items. Live text, thinking, tool arguments, tool progress, and retries have explicit provisional states. A failed or interrupted response never becomes a successful completed bubble merely because some text arrived.

The root conversation contains root messages and concise worker lifecycle/results. It does not interleave every worker token. A compact group strip reports running, waiting, needs-attention, and completed workers and opens the group view. With multi-agent disabled and no retained workers, the strip stays absent.

Disabling new spawning does not remove an existing roster or its controls. Empty group inspection is allowed without enabling model-facing agent tools.

## 3. Agent group and focused worker

The group view supports both a flat roster and a supervision tree. It includes the root explicitly and shows agent identity, assignment, state/wait reason, model, usage, workspace, last activity, and unread attention items. Sorting is stable while navigating; live updates do not move the selection to a different agent.

A wide terminal can place the roster beside the selected transcript or inspector. A narrow terminal switches between them instead of compressing both into unusable columns. Width thresholds are established in prototype P4, not copied from another agent. Search/filter, keyboard focus, and breadcrumbs work in both layouts.

Opening an agent is read-only. It neither resumes, revives, restarts, nor prompts the agent. This is a deliberate difference from Oh My Pi's documented focus-and-revive interaction; its responsive roster and inspector remain useful references. [R7](docs/research.md#references)

A focused worker view shows its conversation, tool activity, pending approvals, assignment, descendants, changes, and effective authority. Sending a message uses the ordinary runtime input path. Returning to the root changes presentation only.

Each agent has its own draft, attachments, cursor, scroll anchor, and selected delivery mode. Focus changes never move a draft to another agent. A command captures its target and expected state at submission; an asynchronous completion cannot act on whichever agent happens to be focused later.

No event automatically steals focus. A worker failure or approval request creates a visible attention item; the user chooses whether to open it.

## 4. Human commands

Bindings are user-configurable and documented from resolved command metadata. Keep command semantics stable before assigning final keys. Proposed command names below describe intent, not a promise of exact spelling or Pi wire compatibility.

| Action | TUI behavior |
|---|---|
| Submit | Shows the target and accepted delivery mode. An accepted receipt is distinct from a finished turn. |
| Steer | Joins the next safe request boundary; never edits the already-sent provider request. |
| Follow-up | Queues a successor turn. The queue remains inspectable and withdrawable before placement. |
| Pause agent/group | Shows pausing while existing effects settle, then paused. Does not claim immediate process suspension. |
| Resume | Explicitly resumes eligible work; reports blocked recovery or missing capabilities. |
| Cancel turn | Names the exact agent and turn; does not silently cancel its retained peers or unrelated jobs. |
| Cancel subtree/group | Previews the affected agents/jobs and uses the runtime's durable scope barrier. |
| Retire agent | Requires descendant and artifact disposition; keeps history inspectable. |
| Restart | Creates fresh execution/identity as defined by the host command and records lineage; never disguises a retry as recovery. |
| Inspect/history/fork | Reads or creates history with explicit context/configuration/authority choices. Does not restore files implicitly. |

Escape first closes a modal or clears a local selection according to the current view. It must not ambiguously mean both leave a worker and cancel that worker. Destructive group actions require an explicit scope selection; an ordinary one-turn cancellation can remain a direct action.

When several clients are attached, replies and notifications are correlated by command ID. Version-sensitive changes use expected revisions. Duplicate approval or assignment decisions return the recorded result or a clear stale/conflict response, not a second action.

## 5. Approvals and questions

An approval always identifies the originating agent, task, tool, workspace/environment, canonical arguments, and requested authority. Mutation previews name actual paths and show a bounded diff where available. Hidden-worker approvals stay reachable from the root through the attention list.

Submitting a decision captures the approval ID and action digest. If arguments, authority, or workspace binding changed, refresh or reject the decision. Never apply a decision to the next item because the list reordered.

Approval state distinguishes waiting, decided, cancelled, expired, and invalidated. A client-local acknowledgement is not proof that the runtime committed the decision. A group-wide allow action must say what scope it grants; approving one worker never silently approves all workers.

Structured questions use a separate interaction from security approval. Closing a question is not equivalent to authorizing a tool. Required user input, missing credentials, quota exhaustion, and a failed tool have distinct presentations.

## 6. Changes and verification

The changes view groups results by agent and workspace. It shows the source base, resulting revision or patch, affected paths, verification commands/results, and whether verification is still applicable to the target checkout.

Review, apply, and cleanup are separate actions. Before application, show target dirty state and possible conflicts. Do not imply that isolated worktrees remove integration conflicts. After application, record verification of the integrated result rather than reusing a worker's earlier green badge.

A completed agent can have unreviewed changes. A verified patch can still be unapproved. A failed agent can leave useful artifacts. These states remain independent.

Worktree/artifact cleanup reports retained versus removed resources and never deletes an unintegrated result as a side effect of closing a panel.

## 7. Lifecycle and lost connections

Transport loss marks views disconnected/stale and disables mutations until reattached. Cached text remains readable. Reconnection obtains a new atomic snapshot and observation epoch; obsolete frames and optimistic controls cannot alter the new view.

A client detaching is not semantic cancellation. An embedded host that is exiting must explicitly suspend/close execution; it cannot advertise background continuation after its process is gone.

On quit with active work, present the actions the current host actually supports: suspend and exit, cancel selected scope and exit, or detach while a persistent host continues. Hide or disable unsupported detach with an explanation. An idle exit requires no unnecessary confirmation.

Crash recovery shows pending, recovering, blocked, and indeterminate outcomes separately. For an uncertain external effect, offer evidence inspection, supported reconciliation, explicit abandonment, or a separately authorized new attempt. No generic retry button that silently repeats a mutating effect.

## 8. Runtime projection and rendering

Use one frontend state owner and reducer-style update path:

```text
input / runtime observation / command reply / resize
    -> update UI state and produce explicit effects
    -> execute effects outside the reducer
    -> render the current view
```

The runtime supplies group summaries and paginated conversation views. The frontend does not infer lifecycle from transcript text, scan artifact directories to construct the authoritative roster, or write storage. Historical display pages and search caches are disposable.

Maintain a bounded group summary subscription and a detailed subscription for the selected agent. Hidden workers can update status/attention without rendering their full transcripts. Whole durable commit envelopes are folded atomically; provisional output uses its channel and invocation cursor. After overflow, reconnect and replace the view rather than guessing which event was missed. [R1](docs/research.md#references)

Coalesce repeated layout and streaming updates. Rendering and input handling never await provider, filesystem, model-catalog, or plugin I/O. Bound render work per frame and virtualize long transcripts/rosters. Preserve a user's scroll anchor while new output arrives; follow-tail is an explicit mode.

Final durable content replaces provisional content by identity, not by matching its text. Reconnect, late delivery, and a fast successor turn must not duplicate an answer or attach old output to the new turn.

## 9. Terminal ownership

One terminal owner manages raw mode, input decoding, bracketed paste, optional keyboard enhancements, mouse mode, alternate screen, suspension, and restoration. Views never write raw escape sequences. Normal exit and errors restore explicitly; Drop/panic restoration is a fallback.

Use grapheme-aware editing and display-width-aware wrapping consistently. Wide characters, combining marks, emoji, tabs, and narrow layouts must not corrupt cursor placement. Reflow comes from logical content; already emitted native scrollback is not treated as mutable UI state.

Sanitize untrusted terminal control sequences in model/tool/plugin output. Hyperlinks and copied text have explicit safe policies. Clipboard export and sharing require deliberate actions and disclose sensitive content; nothing is uploaded automatically.

Mouse and advanced keyboard protocols are optional. Default interactions must work through a basic terminal and tmux. Terminal suspend/resume and external-editor transitions restore/reacquire ownership with a full repaint.

No renderer library or existing Ion abstraction is mandatory. Prototype P4 chooses a rendering approach against these behaviors. It must leave one input owner and one coherent terminal lifecycle rather than layering competing terminal managers.

## 10. Acceptance scenarios

| ID | Scenario | Required result |
|---|---|---|
| U1 | Ordinary single-agent coding | No swarm scaffolding in the model request or UI; core conversation controls work. |
| U2 | Root and two streaming workers | Stable root conversation; each worker inspectable, steerable, and cancellable. |
| U3 | Type in worker A, focus B, receive delayed reply | A's draft/action never routes to B; replies remain correlated. |
| U4 | Hidden worker asks approval during a root prompt | Origin and scope are clear; responding does not alter the root composer. |
| U5 | Group pause/cancel races with spawn | UI reflects committed scope; no worker disappears or escapes unnoticed. |
| U6 | Narrow/wide/narrow resize with multiline and Unicode | All controls stay available; focus, cursor, drafts, and scroll anchors survive. |
| U7 | Output flood and subscription overflow | Input and cancellation remain usable; fresh snapshot repairs state without duplicate final text. |
| U8 | Reopen with many historical agents | Group overview loads without every transcript; recovery state is explicit. |
| U9 | Two isolated workers produce conflicting changes | Both results and base/evidence survive; integration requires an explicit decision. |
| U10 | Clean exit, startup error, panic, external editor, suspend | Terminal restored; runtime cleanup does not silently lose accepted work. |
| U11 | Focus an idle or recovery-blocked worker | Inspection causes no model request or job restart. |
| U12 | Multi-agent disabled after workers exist | Roster, approvals, artifacts, and human control remain available. |

Use reducer tests, deterministic runtime traces, PTY tests, and real-terminal acceptance. Screenshot or golden-frame equality alone does not establish interaction correctness. Performance thresholds are measured prototype outputs, not claims inherited from a previous renderer.
