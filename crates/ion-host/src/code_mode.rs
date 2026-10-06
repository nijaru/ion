//! Confined QuickJS execution. Core owns dispatch, effects, host budgets and audit.

use std::{
    cell::{Cell, RefCell},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use ion_core::{CodeLimits, CodeReply, CodeRequest, CodeRequestKind, CodeRuntime, CodeTask};
use rquickjs::{
    Context, Ctx, Exception, Function, Object, Persistent, Promise, Runtime, Value,
    context::intrinsic,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// A fresh, isolated VM on a blocking worker for each async JavaScript body.
/// No Session, catalogue, module loader, filesystem, network or timers enter it.
#[derive(Debug, Default, Clone, Copy)]
pub struct QuickJs;

impl CodeRuntime for QuickJs {
    fn start(
        &self,
        code: String,
        limits: CodeLimits,
        stop: CancellationToken,
    ) -> Result<CodeTask, String> {
        let setup = (|| {
            let deadline = validate(&code, limits)?;
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| "QuickJS requires a Tokio runtime")?;
            let (sender, requests) = mpsc::channel(limits.max_calls);
            let worker_stop = stop.clone();
            let result = handle.spawn_blocking(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    execute(code, limits, deadline, sender, &worker_stop)
                }))
                .unwrap_or_else(|_| Err("QuickJS worker panicked".into()));
                if result.is_err() {
                    worker_stop.cancel();
                }
                result
            });
            Ok(CodeTask { requests, result })
        })();
        if setup.is_err() {
            stop.cancel();
        }
        setup
    }
}

fn validate(code: &str, limits: CodeLimits) -> Result<Instant, String> {
    if [
        limits.heap_bytes,
        limits.stack_bytes,
        limits.max_code_bytes,
        limits.max_json_bytes,
        limits.max_calls,
        limits.max_concurrency,
        limits.max_host_bytes,
        limits.audit_admission_bytes,
    ]
    .contains(&0)
        || limits.heap_bytes == usize::MAX
        // rquickjs disables stack checking above this ceiling.
        || limits.stack_bytes > 16 * 1024 * 1024
        || limits.deadline.is_zero()
        || limits.max_concurrency > limits.max_calls
        || limits.max_calls > tokio::sync::Semaphore::MAX_PERMITS
    {
        return Err("invalid QuickJS limits".into());
    }
    if code.len() > limits.max_code_bytes {
        return Err("JavaScript code byte limit".into());
    }
    Instant::now()
        .checked_add(limits.deadline)
        .ok_or_else(|| "QuickJS deadline overflow".into())
}

struct Pending {
    reply: oneshot::Receiver<CodeReply>,
    resolve: Persistent<Function<'static>>,
    reject: Persistent<Function<'static>>,
}

struct Bridge {
    sender: mpsc::Sender<CodeRequest>,
    pending: RefCell<Vec<Pending>>,
    admitted: Cell<usize>,
    limits: CodeLimits,
}

fn json_text<'js>(ctx: &Ctx<'js>, value: Value<'js>, limit: usize) -> rquickjs::Result<String> {
    // Stringify (including guest getters/toJSON) is heap/interrupt bounded. The
    // byte cap additionally bounds the JSON transferred across the Rust bridge.
    let text = ctx
        .json_stringify(value)?
        .ok_or_else(|| Exception::throw_type(ctx, "value has no JSON representation"))?
        .to_string()?;
    if text.len() > limit {
        return Err(Exception::throw_range(ctx, "JSON byte limit"));
    }
    Ok(text)
}

fn enqueue<'js>(
    ctx: &Ctx<'js>,
    bridge: &Bridge,
    name: String,
    args: Option<Value<'js>>,
) -> rquickjs::Result<Promise<'js>> {
    if name.len() > bridge.limits.max_json_bytes {
        return Err(Exception::throw_range(ctx, "bridge name byte limit"));
    }
    let args_json = args
        .map(|v| json_text(ctx, v, bridge.limits.max_json_bytes))
        .transpose()?;
    // Serialization may reenter tools.call via toJSON/getters. Never reserve
    // capacity or hold a RefCell borrow across guest execution.
    if bridge.admitted.get() >= bridge.limits.max_calls
        || bridge.pending.borrow().len() >= bridge.limits.max_calls
    {
        return Err(Exception::throw_range(ctx, "bridge call limit"));
    }
    let (promise, resolve, reject) = Promise::new(ctx)?;
    let (reply, receiver) = oneshot::channel();
    let kind = match args_json {
        Some(args_json) => CodeRequestKind::Call { name, args_json },
        None => CodeRequestKind::Describe { name },
    };
    // A native callback must never block: blocking would evade the interrupt.
    bridge
        .sender
        .try_send(CodeRequest { kind, reply })
        .map_err(|_| Exception::throw_message(ctx, "host queue full or closed"))?;
    bridge.admitted.set(bridge.admitted.get() + 1);
    bridge.pending.borrow_mut().push(Pending {
        reply: receiver,
        resolve: Persistent::save(ctx, resolve),
        reject: Persistent::save(ctx, reject),
    });
    Ok(promise)
}

fn js_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if error.is_exception() {
        // Even an Error's message/stack can be a hostile getter. Do not read
        // properties or coerce guest values while reporting a failure.
        drop(ctx.catch());
        "JavaScript exception".into()
    } else {
        // Library errors contain no guest object coercion; still bound text.
        let mut text = error.to_string();
        let mut end = text.len().min(256);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text
    }
}

fn start_guest<'js>(
    ctx: Ctx<'js>,
    code: &str,
    bridge: &Rc<Bridge>,
) -> rquickjs::Result<Persistent<Promise<'static>>> {
    ctx.globals().remove("queueMicrotask")?;
    let calls = bridge.clone();
    // Ctx must be injected per invocation, not captured in an opaque Rust
    // closure: captured contexts hide GC edges and can prevent VM teardown.
    let call = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, name: String, args: Value<'js>| {
            enqueue(&ctx, &calls, name, Some(args))
        },
    )?;
    let descriptions = bridge.clone();
    let describe = Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String| {
        enqueue(&ctx, &descriptions, name, None)
    })?;
    let tools = Object::new(ctx.clone())?;
    tools.set("call", call)?;
    tools.set("describe", describe)?;
    let input: Function = ctx.eval(format!("(async (tools) => {{\n{code}\n}})"))?;
    let root: Promise = input.call((tools,))?;
    Ok(Persistent::save(&ctx, root))
}

fn poll_replies(ctx: &Ctx<'_>, bridge: &Bridge) -> rquickjs::Result<()> {
    let mut ready = Vec::new();
    {
        let mut pending = bridge.pending.borrow_mut();
        let mut index = 0;
        while index < pending.len() {
            let reply = match pending[index].reply.try_recv() {
                Ok(reply) => reply,
                Err(oneshot::error::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(oneshot::error::TryRecvError::Closed) => Err("host reply closed".into()),
            };
            ready.push((pending.swap_remove(index), reply));
        }
    }
    // Resolving can execute a guest `then` getter and reenter the bridge.
    for (item, reply) in ready {
        match reply {
            Ok(text) if text.len() <= bridge.limits.max_json_bytes => {
                let value = ctx.json_parse(text)?;
                item.resolve.restore(ctx)?.call::<_, ()>((value,))?;
            }
            Ok(_) => {
                item.reject
                    .restore(ctx)?
                    .call::<_, ()>(("host JSON byte limit",))?;
            }
            Err(error) => {
                let error = if error.len() <= bridge.limits.max_json_bytes {
                    error
                } else {
                    "host error byte limit".into()
                };
                item.reject.restore(ctx)?.call::<_, ()>((error,))?;
            }
        }
    }
    Ok(())
}

fn execute(
    code: String,
    limits: CodeLimits,
    deadline: Instant,
    sender: mpsc::Sender<CodeRequest>,
    stop: &CancellationToken,
) -> CodeReply {
    // Blocking-pool admission may itself outlast the guest budget.
    if stop.is_cancelled() {
        return Err("JavaScript cancelled".into());
    }
    if Instant::now() >= deadline {
        return Err("JavaScript deadline exceeded".into());
    }
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(limits.heap_bytes);
    runtime.set_max_stack_size(limits.stack_bytes);
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal = interrupted.clone();
    let interrupt_stop = stop.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let expired = Instant::now() >= deadline || interrupt_stop.is_cancelled();
        if expired {
            signal.store(true, Ordering::Relaxed);
        }
        expired
    })));
    // Eval is needed to compile the body; no host APIs or module loader exist.
    let context =
        Context::custom::<(intrinsic::Eval, intrinsic::Json, intrinsic::Promise)>(&runtime)
            .map_err(|e| e.to_string())?;
    let bridge = Rc::new(Bridge {
        sender,
        pending: RefCell::new(Vec::new()),
        admitted: Cell::new(0),
        limits,
    });
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let root = context
            .with(|ctx| start_guest(ctx, &code, &bridge))
            .map_err(|e| context.with(|ctx| js_error(&ctx, e)))?;
        loop {
            if stop.is_cancelled() {
                return Err("JavaScript cancelled".into());
            }
            if Instant::now() >= deadline || interrupted.load(Ordering::Relaxed) {
                return Err("JavaScript deadline exceeded".into());
            }
            let result = context
                .with(|ctx| {
                    let root = root.clone().restore(&ctx)?;
                    // Observe rejection before settling any more bridge replies.
                    if let Some(value) = root.result::<Value>() {
                        return value
                            .and_then(|v| json_text(&ctx, v, limits.max_json_bytes))
                            .map(Some);
                    }
                    poll_replies(&ctx, &bridge)?;
                    Ok(None)
                })
                .map_err(|e| context.with(|ctx| js_error(&ctx, e)))?;
            if let Some(json) = result {
                return Ok(json);
            }
            // Outside Context::with: this API acquires the same runtime lock.
            match runtime.execute_pending_job() {
                Ok(true) => (),
                Ok(false) => thread::sleep(Duration::from_millis(1)),
                Err(job) => {
                    return Err(job.0.with(|ctx| js_error(&ctx, rquickjs::Error::Exception)));
                }
            }
        }
    }))
    .unwrap_or_else(|_| Err("QuickJS worker panicked".into()));
    let outcome = if stop.is_cancelled() {
        Err("JavaScript cancelled".into())
    } else if interrupted.load(Ordering::Relaxed) || Instant::now() >= deadline {
        Err("JavaScript deadline exceeded".into())
    } else {
        outcome
    };
    if outcome.is_err() {
        // Signal Core before VM teardown/closing the sender so queued requests
        // cannot be mistaken for work to dispatch after root failure.
        stop.cancel();
    }
    // Every Persistent must die before Context/Runtime, including on failure.
    // Successful exit only drops reply receivers, never transferred requests.
    bridge.pending.borrow_mut().clear();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> CodeLimits {
        CodeLimits {
            heap_bytes: 8 * 1024 * 1024,
            stack_bytes: 256 * 1024,
            deadline: Duration::from_secs(2),
            max_json_bytes: 4096,
            ..CodeLimits::default()
        }
    }

    fn start(code: &str, limits: CodeLimits) -> (CodeTask, CancellationToken) {
        let stop = CancellationToken::new();
        let task = QuickJs.start(code.into(), limits, stop.clone()).unwrap();
        (task, stop)
    }

    async fn finish(task: &mut CodeTask) -> CodeReply {
        tokio::time::timeout(Duration::from_secs(3), &mut task.result)
            .await
            .expect("worker must terminate")
            .expect("worker must contain panics")
    }

    async fn request(task: &mut CodeTask) -> CodeRequest {
        tokio::time::timeout(Duration::from_secs(3), task.requests.recv())
            .await
            .expect("bridge must not block")
            .expect("request expected")
    }

    #[tokio::test]
    async fn awaited_json_calls_and_describe() {
        let (mut task, stop) = start(
            "const a = await tools.call('echo', {n: 1}); const b = await tools.describe('ec'); return {a, b};",
            limits(),
        );
        let call = request(&mut task).await;
        assert!(
            matches!(call.kind, CodeRequestKind::Call { name, args_json }
            if name == "echo" && args_json == "{\"n\":1}")
        );
        call.reply
            .send(Ok("{\"value\":2,\"is_error\":false}".into()))
            .unwrap();
        let describe = request(&mut task).await;
        assert!(matches!(describe.kind, CodeRequestKind::Describe { name } if name == "ec"));
        describe.reply.send(Ok("[\"echo\"]".into())).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&finish(&mut task).await.unwrap()).unwrap(),
            json!({"a": {"value": 2, "is_error": false}, "b": ["echo"]}),
        );
        assert!(!stop.is_cancelled());
        assert!(task.requests.recv().await.is_none());
    }

    #[tokio::test]
    async fn unawaited_transfer_survives_successful_return() {
        let (mut task, stop) = start("void tools.call('effect', {n: 1}); return 7;", limits());
        assert_eq!(finish(&mut task).await, Ok("7".into()));
        assert!(!stop.is_cancelled());
        let call = request(&mut task).await;
        assert!(matches!(call.kind, CodeRequestKind::Call { name, .. } if name == "effect"));
        // Core still owns this transferred request even though its guest no
        // longer consumes the reply. Neither success nor receiver loss cancels.
        assert!(call.reply.send(Ok("null".into())).is_err());
        assert!(task.requests.recv().await.is_none());
        assert!(!stop.is_cancelled());
    }

    #[tokio::test]
    async fn deadlines_bound_cpu_idle_promises_and_jobs() {
        for code in [
            "while (true) {}",
            "await Promise.resolve(); while (true) {}",
            "await new Promise(() => {});",
            "const again = () => Promise.resolve().then(again); again(); await new Promise(() => {});",
        ] {
            let (mut task, stop) = start(
                code,
                CodeLimits {
                    deadline: Duration::from_millis(30),
                    ..limits()
                },
            );
            let began = Instant::now();
            assert_eq!(
                finish(&mut task).await,
                Err("JavaScript deadline exceeded".into()),
                "{code}"
            );
            assert!(began.elapsed() < Duration::from_secs(1), "{code}");
            assert!(stop.is_cancelled());
            assert!(task.requests.recv().await.is_none());
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_awaited_host_and_bare_promise() {
        let stop = CancellationToken::new();
        stop.cancel();
        let mut task = QuickJs
            .start(
                "void tools.call('must not transfer', {}); return 0;".into(),
                limits(),
                stop,
            )
            .unwrap();
        assert_eq!(finish(&mut task).await, Err("JavaScript cancelled".into()));
        assert!(task.requests.recv().await.is_none());

        for code in [
            "await tools.call('waiting', {});",
            "await new Promise(() => {});",
        ] {
            let (mut task, stop) = start(code, limits());
            let held = if code.contains("tools.call") {
                Some(request(&mut task).await)
            } else {
                tokio::time::sleep(Duration::from_millis(10)).await;
                None
            };
            stop.cancel();
            assert_eq!(finish(&mut task).await, Err("JavaScript cancelled".into()));
            assert!(task.requests.recv().await.is_none());
            drop(held);
        }
    }

    #[tokio::test]
    async fn failures_cancel_and_error_reporting_never_runs_guest_getters() {
        for code in [
            "return (;", // parse failure
            "return undefined;",
            "const a = {}; a.a = a; return a;",
            "return 1n;",
            "return 'x'.repeat(5000);",
            "await tools.call('bad', 'x'.repeat(5000));",
            "const a = []; while (true) a.push('x'.repeat(65536));",
            "function recur() { return recur(); } recur();",
            "const e = new Error(); Object.defineProperty(e, 'message', {get() {while(true) {}}}); throw e;",
            "throw {toString() {while(true) {}}};",
            "await import('node:fs');",
        ] {
            let (mut task, stop) = start(code, limits());
            let began = Instant::now();
            let error = finish(&mut task).await.unwrap_err();
            assert!(error.len() <= 256, "{code}: {error}");
            assert!(began.elapsed() < Duration::from_secs(1), "{code}");
            assert!(stop.is_cancelled(), "{code}");
            assert!(task.requests.recv().await.is_none(), "{code}");
        }
    }

    #[tokio::test]
    async fn only_the_tools_bridge_has_host_capabilities() {
        let (mut task, stop) = start(
            "const absent = ['process','require','module','exports','fs','fetch','XMLHttpRequest','WebSocket',
                'setTimeout','setInterval','setImmediate','queueMicrotask','console','Deno','Bun','std','os'];
             return {absent: absent.every(k => typeof globalThis[k] === 'undefined'),
                     bridge: Object.keys(tools).sort()};",
            limits(),
        );
        assert_eq!(
            finish(&mut task).await.unwrap(),
            "{\"absent\":true,\"bridge\":[\"call\",\"describe\"]}"
        );
        assert!(!stop.is_cancelled());
    }

    #[tokio::test]
    async fn serialization_reentry_cannot_exceed_admission() {
        let (mut task, stop) = start(
            "try { await tools.call('outer', {toJSON() { void tools.call('inner', {}); return {}; }}); }
             catch (e) { return e.message; }",
            CodeLimits { max_calls: 1, max_concurrency: 1, ..limits() },
        );
        assert_eq!(finish(&mut task).await, Ok("\"bridge call limit\"".into()));
        let call = request(&mut task).await;
        assert!(matches!(call.kind, CodeRequestKind::Call { name, .. } if name == "inner"));
        assert!(task.requests.recv().await.is_none());
        assert!(!stop.is_cancelled());
    }

    #[tokio::test]
    async fn resolving_host_json_can_reenter_without_borrowing_pending() {
        let (mut task, stop) = start(
            "let fired = false;
             Object.defineProperty(Object.prototype, 'then', {configurable: true, get() {
                 if (!fired) { fired = true; void tools.describe('nested'); } return undefined;
             }});
             const a = await tools.call('first', {}); delete Object.prototype.then; return a;",
            limits(),
        );
        let call = request(&mut task).await;
        call.reply.send(Ok("{\"n\":1}".into())).unwrap();
        assert_eq!(finish(&mut task).await, Ok("{\"n\":1}".into()));
        let nested = request(&mut task).await;
        assert!(matches!(nested.kind, CodeRequestKind::Describe { name } if name == "nested"));
        assert!(task.requests.recv().await.is_none());
        assert!(!stop.is_cancelled());
    }

    #[tokio::test]
    async fn total_calls_are_bounded_even_after_replies_settle() {
        let (mut task, stop) = start(
            "for (let i = 0; i < 3; i++) await tools.call('next', {i}); return true;",
            CodeLimits {
                max_calls: 2,
                max_concurrency: 1,
                ..limits()
            },
        );
        for i in 0..2 {
            let call = request(&mut task).await;
            assert!(matches!(call.kind, CodeRequestKind::Call { args_json, .. }
                if serde_json::from_str::<serde_json::Value>(&args_json).unwrap() == json!({"i": i})));
            call.reply.send(Ok("null".into())).unwrap();
        }
        assert!(finish(&mut task).await.is_err());
        assert!(stop.is_cancelled());
        assert!(task.requests.recv().await.is_none());
    }

    #[tokio::test]
    async fn root_failure_signals_stop_before_completion_with_queued_work() {
        let (mut task, stop) = start(
            "void tools.call('queued', {}); throw new Error('failed');",
            limits(),
        );
        tokio::time::timeout(Duration::from_secs(1), stop.cancelled())
            .await
            .unwrap();
        assert!(finish(&mut task).await.is_err());
        let queued = request(&mut task).await;
        assert!(matches!(queued.kind, CodeRequestKind::Call { name, .. } if name == "queued"));
        assert!(task.requests.recv().await.is_none());
    }

    #[tokio::test]
    async fn host_reply_boundaries_fail_without_hanging() {
        for reply in [
            Some(Ok("x".repeat(5000))),
            Some(Ok("invalid JSON".into())),
            Some(Err("x".repeat(5000))),
            None,
        ] {
            let (mut task, stop) = start("return await tools.call('host', {});", limits());
            let call = request(&mut task).await;
            if let Some(reply) = reply {
                call.reply.send(reply).unwrap();
            } else {
                drop(call.reply);
            }
            assert!(finish(&mut task).await.is_err());
            assert!(stop.is_cancelled());
        }
    }

    #[tokio::test]
    async fn invalid_limits_and_oversized_code_are_rejected_before_start() {
        let base = limits();
        for limits in [
            CodeLimits {
                heap_bytes: 0,
                ..base
            },
            CodeLimits {
                heap_bytes: usize::MAX,
                ..base
            },
            CodeLimits {
                stack_bytes: 0,
                ..base
            },
            CodeLimits {
                stack_bytes: 16 * 1024 * 1024 + 1,
                ..base
            },
            CodeLimits {
                deadline: Duration::ZERO,
                ..base
            },
            CodeLimits {
                deadline: Duration::MAX,
                ..base
            },
            CodeLimits {
                max_code_bytes: 0,
                ..base
            },
            CodeLimits {
                max_json_bytes: 0,
                ..base
            },
            CodeLimits {
                max_calls: 0,
                ..base
            },
            CodeLimits {
                max_calls: usize::MAX,
                ..base
            },
            CodeLimits {
                max_concurrency: 0,
                ..base
            },
            CodeLimits {
                max_concurrency: base.max_calls + 1,
                ..base
            },
            CodeLimits {
                max_host_bytes: 0,
                ..base
            },
            CodeLimits {
                audit_admission_bytes: 0,
                ..base
            },
            CodeLimits {
                max_code_bytes: 1,
                ..base
            },
        ] {
            let stop = CancellationToken::new();
            assert!(
                QuickJs
                    .start("return 0;".into(), limits, stop.clone())
                    .is_err(),
                "{limits:?}"
            );
            assert!(stop.is_cancelled());
        }
    }
}
