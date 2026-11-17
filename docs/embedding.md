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

## Input and attachment authority

Application input uses `UserInput::Prompt { text, attachments }` and `ion_app::attachments::AttachmentSource`: automatic files, image-only files, owned image bytes, prepared images and literal text snapshots. `AttachmentSource::image_bytes(bytes)` avoids base64 in Rust callers.

Custom runtimes default to no filesystem attachment access. Explicitly set `ApplicationRuntime::allow_file_attachments` to permit file sources; in-memory sources need no filesystem permission. Core receives only prepared typed `Message` content, never source paths. The direct `prepare_attachments` helper requires absolute paths; `SessionControl` resolves relative sources against its Session.

Pending sources and prepared messages share a 32 MiB encoded custody budget. A locally queued input can still be returned if preparation exceeds that budget or the model's limits. A local input ID or pending launch is not durable acceptance. Use Core input/request receipts and reliable control updates to distinguish committed, not accepted and unresolved inputs. Never automatically retry or restore an unresolved input as a fresh request.

## Cancellation and resource lifetime

Core execution APIs treat a caller's cancellation token as a parent scope. Cancelling it stops all operations beneath it. Stopping or dropping one `Operation` stops only that operation and its descendants, not the parent or sibling Sessions. Dropping an operation handle requests stop but does not join it. Retain and await the handle, or join through Session shutdown, before releasing host resources its work uses.

`Session::create/open` and `SessionSetup::new` are blocking factory operations. Async coding clients use retained `CodingApplication::prepare_setup`, Session I/O and reader capabilities. Keep `setup.retirements()` outside the controller. After controller shutdown, drain reliable updates, release every Session/read capability, then await the retirement scope's `join()`.

Shutdown preserves read-only access; retirement separately joins native SQLite close and writer-lease release. Cancelling a borrowed wait does not cancel or discard its native work.

## Tool output and retained captures

`ToolOutput` includes typed `artifacts`; observed shell outcomes retain the whole output. Low-level tool execution takes an explicit `ToolInvocation`. Outside admitted execution, callers must supply an artifact sink to retain captures. These unreleased APIs have no compatibility shim. For retention, copy and backup behavior, see [Session captures](sessions.md#retained-evidence-and-captures).

Embedded tool-veto interfaces and their authority limits are documented in [tool control](tool-control.md#embedded-hosts). Tool selection is not host isolation, and cancellation does not undo effects.
