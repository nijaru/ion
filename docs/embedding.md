# Rust embedding

`CodingApplication` and `SessionControl` expose the engine used by the coding clients. For application entry points and examples, see [`crates/ion-cli/examples/`](../crates/ion-cli/examples/). For a lower-level, non-coding host, start with the [Core API example](../crates/ion-core/src/lib.rs). These are unreleased APIs with no compatibility commitment.

Generate Core's reference locally:

```sh
cargo doc --locked -p ion-core --no-deps
```

## Crate responsibilities

- `ion-ai`: model-facing types.
- `ion-core`: Turn and Session runtime.
- `ion-app`: shared application, SDK and effect executors.
- `ion-terminal`: physical terminal.
- `crates/ion-cli`: the `ion` executable's headless, terminal and RPC clients.

These are current code owners, not promised permanent API boundaries. The [README](../README.md#develop) lists workspace verification commands.

## Host and protocol boundaries

Native and embedded clients use the same Rust engine. Ion's design separates model access, workspace and tools, storage, and wakeups from Turn decisions. Native Sessions use SQLite; shell tools use native processes. The [Workerd journal fixture](../hosts/cloudflare/README.md) exercises a different storage host but does not run the Agent or its tools.

`ion rpc` is not [ACP](https://agentclientprotocol.com/). ACP support is planned in two directions: editors could drive Ion as an ACP agent, or Ion could connect to an external ACP agent. In the latter case the external agent owns the loop, history, and authentication. An optional MCP bridge would let it call explicitly granted Ion tools through Ion's own validation and permissions. That does not put the agent's other tools or recovery under Ion's control. Neither ACP direction nor tool export is available today.

## Input and attachment authority

Application input uses `UserInput::Prompt { text, attachments }` and `ion_app::attachments::AttachmentSource`: automatic files, image-only files, owned image bytes, prepared images and literal text snapshots. `AttachmentSource::image_bytes(bytes)` avoids base64 in Rust callers.

Custom runtimes default to no filesystem attachment access. Explicitly set `ApplicationRuntime::allow_file_attachments` to permit file sources; in-memory sources need no filesystem permission. Core receives only prepared typed `Message` content, never source paths. The direct `prepare_attachments` helper requires absolute paths; `SessionControl` resolves relative sources against its Session.

`SessionControl::queued_inputs(preview_chars)` projects unaccepted steering and follow-ups from their custody owner, with bounded original-text previews and attachment counts rather than payload copies. Its `withdrawable` field samples whether custody is still available to withdraw; a concurrent Core claim can change that before `clear_queue()` runs. `clear_queue()` requests withdrawal of that whole queue, not the active root; only returned inputs establish nonacceptance. Already committed inputs are not restored or retried.

Pending sources and prepared messages share a 32 MiB encoded custody budget. A locally queued input can still be returned if preparation exceeds that budget or the model's limits. A local input ID or pending launch is not durable acceptance. Use Core input/request receipts and reliable control updates to distinguish committed, not accepted and unresolved inputs. Never automatically retry or restore an unresolved input as a fresh request. Transferring an `UnresolvedInput` frees its admission slot, but its byte reservation remains charged while the recipient retains the wrapper; dropping it releases local custody, not proof of rollback.

## Model access

`Host::agent` constructs a model/tool binding without requiring credentials to be present. Core calls `ModelService::validate_access` before admitting a new user Turn or explicit Turn recovery. Failure leaves a new Turn's input unaccepted; an already-committed keyed request can still be reconciled without current access. Passive context reports do not check credentials.

Anonymous and synthetic services may use the default access preflight. Forwarding services must delegate it. HTTP routes that require credentials reject a missing key both during preflight and at dispatch. Dispatch resolves credentials again: preflight is not an atomic grant, and logout or revocation can prevent a later request. Credentials are never Session facts.

`ion_app::auth::chatgpt` implements the callback and signed grant exchange for [Sign in with ChatGPT](https://developers.openai.com/siwc/token-sharing-open-source). For native embedding, use `ProfileStore` with a dedicated owner-only directory on a local filesystem. Get its `host_id()` before registration, `remember()` the issued client before exchanging the code, and `install()` only after the exchange verifies the grant. For reauthorization, load the saved registration with `profile()`; do not register a new client for the same account.

Resolve `access()` before each request. The store serializes refresh across processes and saves token replacements before returning them. After a refresh with an unknown result, sign in again with the same registration; the store won't replay the old token. Stop the host's in-flight requests before `disconnect()`. If it returns `RemoteUnconfirmed`, local tokens are gone but the user may still need to disconnect the app in [ChatGPT Settings](https://chatgpt.com/settings/usage). The native store uses Unix file permissions, not encryption; don't put it in shared storage or restore it from a snapshot.

This SDK has been tested against loopback fixtures only. It does not launch a browser or bind a model route. The CLI still uses API keys for OpenAI Responses.

## Cancellation and resource lifetime

Core execution APIs treat a caller's cancellation token as a parent scope. Cancelling it stops all operations beneath it. Stopping or dropping one `Operation` stops only that operation and its descendants, not the parent or sibling Sessions. Dropping an operation handle requests stop but does not join it. Retain and await the handle, or join through Session shutdown, before releasing host resources its work uses.

Async clients use `CodingApplication::open_session` or `SessionSetup::open`. The retained setup handle acquires the writer and builds the binding off the executor. `request_stop()` is cooperative; join the handle even after requesting stop.

Use `setup.into_control()` when the application is shared: shutdown stops only that controller's Session. A standalone client uses `setup.into_application_control()`. Its shutdown also closes and joins application-owned subagents and processes. Both return pending control updates and cleanup errors; a cleanup failure does not replace an earlier operation failure.

Use `control.begin_close()` and join its retained handle when the client no longer needs the live binding. A standalone close returns after SQLite closes and releases the writer lease. Drop live Session, reader and I/O handles first; a passive `SessionHistory` receipt handle can stay alive. For a shared application, the outer owner still joins its retirement scope after shutting down all controls and transferred work. Clients that need final live reads can first use borrowed `shutdown()`, then close. Cancelling a borrowed wait does not abandon native work.


## Keyed wakeups

A single-Session application can use `SessionControl<()>::submit_with_wake` for scheduled work. Bind the Agent with `with_recovery_scope`, authorize the request key, and supply a callback that acknowledges the durable wakeup only after recording its target and scheduling it. Ion calls the callback before accepting new work. Redelivery of a committed key returns its receipt without changing the wakeup.

On a valid alarm, pass its `WakeTarget` and a rearm callback to `recover_from_wake`. Ion continues only the unfinished Turn owned by that key. It rearms before resuming or reporting that the current instance already owns the work. `Quiescent` means there is no work for that target; an alarm never creates an input. `Refused` preserves whether continuation was rejected or its commit is uncertain. Inspect uncertain outcomes before proceeding.

The application owns authentication, the durable scheduler, and serialization across delivery and alarms. Fence stale alarms and clear only the wake generation you still own. These methods do not supply a Durable Object runtime. The current test uses native process loss and a file-backed wake fixture.

## Tool schemas

Core compiles each frozen catalog entry's input schema once and shares that immutable contract when rebuilding exposure. All dispatch paths enforce it before policy or effects. See [registered input schemas](tool-control.md#registered-input-schemas) for strict, offline checks and supported bounds.

`ToolResultSchema` remains discovery metadata, not general runtime result enforcement. MCP's `outputSchema` has separate reporting: App uses the same Core compiler to annotate successful remote observations with `valid`, `invalid`, `indeterminate` or `not_checked`; remote errors use `skipped_remote_error`. Missing structured content differs from an explicit JSON null. An unsupported output schema does not disable the tool's input contract or turn its observed success into an executor failure. Invalid, inconclusive and unchecked output reports retain raw response evidence through the artifact sink, including when inline output is truncated. Reporting never rewrites the server response or replays the request.

## Tool output and retained captures

`ToolOutput` includes typed `artifacts`; observed shell outcomes retain the whole output. Low-level tool execution takes an explicit `ToolInvocation`. Outside admitted execution, callers must supply an artifact sink to retain captures. These unreleased APIs have no compatibility shim. For retention, copy and backup behavior, see [Session captures](sessions.md#retained-evidence-and-captures).

Embedded tool-veto interfaces and their authority limits are documented in [tool control](tool-control.md#embedded-hosts). Tool selection is not host isolation, and cancellation does not undo effects.
