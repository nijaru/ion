//! Confined QuickJS execution. Core owns dispatch, effects, host budgets and audit.

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use ion_core::{
    CodeLimits, CodeReply, CodeRequest, CodeRequestKind, CodeResult, CodeRuntime, CodeTask,
};
use rquickjs::{
    Context, Ctx, Exception, Function, Object, Persistent, Promise, Runtime, Value,
    context::intrinsic,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

mod output;

/// A fresh, isolated VM on an owned thread for each async JavaScript body.
/// Only owned code and callable-name data enter it, never a Session or executable
/// catalogue. No module loader, filesystem, network or timers are available.
#[derive(Debug, Default, Clone, Copy)]
pub struct QuickJs;

impl CodeRuntime for QuickJs {
    fn start(
        &self,
        code: String,
        tools_json: String,
        limits: CodeLimits,
        stop: CancellationToken,
    ) -> Result<CodeTask, String> {
        let setup = (|| {
            validate(&code, &tools_json, limits)?;
            let (sender, requests) = mpsc::channel(limits.max_calls);
            let (completed, completion) = oneshot::channel();
            let worker_stop = stop.clone();
            // A guest awaiting native/MCP work must not reserve one of that
            // work's blocking-pool slots. The admitted invocation owns its VM
            // thread; no pool or detached result task sits between it and Core.
            let worker = thread::Builder::new()
                .name("ion-code-vm".into())
                // Preserve room for Rust/bootstrap frames beyond the checked
                // guest stack, including callers choosing above-default limits.
                .stack_size(limits.stack_bytes + 2 * 1024 * 1024)
                .spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        execute(code, tools_json, limits, sender, &worker_stop)
                    }))
                    .unwrap_or_else(|_| CodeResult {
                        result: Err("QuickJS worker panicked".into()),
                        output: Vec::new(),
                    });
                    if result.result.is_err() {
                        worker_stop.cancel();
                    }
                    let _ = completed.send(result);
                })
                .map_err(|error| format!("cannot start QuickJS worker: {error}"))?;
            let result = Box::pin(async move {
                let result = completion.await;
                // execute has already destroyed the VM, bridge and output
                // owner before signalling. Only thread exit remains; no host
                // work or guest teardown can block this join.
                worker
                    .join()
                    .map_err(|_| "QuickJS worker panicked".to_owned())?;
                result.map_err(|_| "QuickJS worker exited without a result".into())
            });
            Ok(CodeTask { requests, result })
        })();
        if setup.is_err() {
            stop.cancel();
        }
        setup
    }
}

fn validate(code: &str, tools_json: &str, limits: CodeLimits) -> Result<(), String> {
    if [
        limits.heap_bytes,
        limits.stack_bytes,
        limits.max_code_bytes,
        limits.max_json_bytes,
        limits.max_output_bytes,
        limits.max_calls,
        limits.max_concurrency,
        limits.max_host_bytes,
        limits.audit_admission_bytes,
    ]
    .contains(&0)
        || limits.heap_bytes == usize::MAX
        // rquickjs disables stack checking above this ceiling.
        || limits.stack_bytes > 16 * 1024 * 1024
        || limits.execution_budget.is_zero()
        || limits.max_concurrency > limits.max_calls
        || limits.max_calls > tokio::sync::Semaphore::MAX_PERMITS
    {
        return Err("invalid QuickJS limits".into());
    }
    if code.len() > limits.max_code_bytes {
        return Err("JavaScript code byte limit".into());
    }
    if tools_json.len() > limits.max_json_bytes.min(limits.max_host_bytes) {
        return Err("code_mode tool manifest byte limit exceeded".into());
    }
    let names: Vec<String> = serde_json::from_str(tools_json)
        .map_err(|_| "code_mode tool manifest must be an array of names")?;
    let mut seen = HashSet::new();
    for name in &names {
        if matches!(name.as_str(), "then" | "toJSON") {
            return Err(format!(
                "code_mode cannot expose reserved tool name {name:?}"
            ));
        }
        if !seen.insert(name) {
            return Err("code_mode tool manifest contains duplicate names".into());
        }
    }
    Instant::now()
        .checked_add(limits.execution_budget)
        .map(|_| ())
        .ok_or_else(|| "QuickJS execution budget overflow".into())
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
    kind: CodeRequestKind,
) -> rquickjs::Result<Promise<'js>> {
    if let CodeRequestKind::Call { name, .. }
    | CodeRequestKind::DescribeTool { name }
    | CodeRequestKind::SearchTools { query: name, .. } = &kind
        && name.len() > bridge.limits.max_json_bytes
    {
        return Err(Exception::throw_range(ctx, "bridge name byte limit"));
    }
    // Guest serialization occurs before admission: toJSON/getters can reenter
    // any bridge method. Never reserve or borrow across guest execution.
    if bridge.admitted.get() >= bridge.limits.max_calls
        || bridge.pending.borrow().len() >= bridge.limits.max_calls
    {
        return Err(Exception::throw_range(ctx, "bridge call limit"));
    }
    let (promise, resolve, reject) = Promise::new(ctx)?;
    let (reply, receiver) = oneshot::channel();
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

// Private captured builtins: never published, and only used on strings or
// native Error values, not Proxy wrappers. Descriptor inspection runs no getters.
fn diagnostic_helper(ctx: &Ctx<'_>) -> rquickjs::Result<Persistent<Function<'static>>> {
    let helper: Function = ctx.eval(
        r#"(() => {
        const own = Object.getOwnPropertyDescriptor;
        const slice = Function.prototype.call.bind(String.prototype.slice);
        const unit = Function.prototype.call.bind(String.prototype.charCodeAt);
        return value => {
            if (typeof value !== 'string') {
                const message = own(value, 'message');
                if (!message) return undefined;
                const data = own(message, 'value');
                if (!data || typeof data.value !== 'string') return undefined;
                value = data.value;
            }
            const text = slice(value, 0, 256);
            const last = unit(text, text.length - 1);
            // A UTF-16 slice can split a supplementary character. Remove the
            // partial surrogate before Rust's UTF-8 conversion.
            return last >= 0xd800 && last <= 0xdbff ? slice(text, 0, -1) : text;
        };
    })()"#,
    )?;
    Ok(Persistent::save(ctx, helper))
}

fn js_error(
    ctx: &Ctx<'_>,
    error: rquickjs::Error,
    helper: Option<&Persistent<Function<'static>>>,
) -> String {
    if error.is_exception() {
        let value = ctx.catch();
        if (value.is_string() || value.is_error())
            && let Some(helper) = helper
        {
            let message = helper
                .clone()
                .restore(ctx)
                .and_then(|helper| helper.call::<_, Value>((value,)));
            if let Ok(message) = message {
                if let Some(message) = message.as_string()
                    && let Ok(mut text) = message.to_string()
                {
                    // Slice first: conversion copies at most 256 UTF-16 units,
                    // never an arbitrarily large guest message.
                    let mut end = text.len().min(256);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    if !text.is_empty() {
                        return text;
                    }
                }
            } else {
                drop(ctx.catch());
            }
        }
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
    tools_json: &str,
    bridge: &Rc<Bridge>,
    output: &Rc<output::Output>,
) -> rquickjs::Result<Persistent<Promise<'static>>> {
    ctx.globals().remove("queueMicrotask")?;
    output.install(&ctx)?;
    let calls = bridge.clone();
    // Ctx must be injected per invocation, not captured in an opaque Rust
    // closure: captured contexts hide GC edges and can prevent VM teardown.
    let call = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, name: String, args: Value<'js>| {
            let args_json = json_text(&ctx, args, calls.limits.max_json_bytes)?;
            enqueue(&ctx, &calls, CodeRequestKind::Call { name, args_json })
        },
    )?;
    let descriptions = bridge.clone();
    let describe = Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String| {
        enqueue(&ctx, &descriptions, CodeRequestKind::DescribeTool { name })
    })?;
    let inspections = bridge.clone();
    let inspect = Function::new(ctx.clone(), move |ctx: Ctx<'js>, query: Value<'js>| {
        let query_json = json_text(&ctx, query, inspections.limits.max_json_bytes)?;
        enqueue(&ctx, &inspections, CodeRequestKind::Inspect { query_json })
    })?;
    // Install only exact frozen names. Alias normalization would make punctuation
    // and case-distinct registrations compete for another tool's executable route.
    let namespace: Function = ctx.eval(
        r#"((names, dispatch) => {
            const tools = Object.create(null);
            for (const name of names) {
                Object.defineProperty(tools, name, {
                    value: args => dispatch(name, args), enumerable: true
                });
            }
            return Object.freeze(tools);
        })"#,
    )?;
    let names: Value = ctx.json_parse(tools_json)?;
    let tools: Object = namespace.call((names, call))?;
    ctx.globals().set("describeTool", describe)?;
    let searches = bridge.clone();
    let search = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, query: String, limit: usize| {
            enqueue(
                &ctx,
                &searches,
                CodeRequestKind::SearchTools { query, limit },
            )
        },
    )?;
    // Capture validation builtins before guest code can replace globals. Read
    // options before enqueue: getters may reenter, but cannot reserve admission.
    let search_wrapper: Function = ctx.eval(
        r#"(bridge => {
        const names = Object.getOwnPropertyNames, symbols = Object.getOwnPropertySymbols;
        const array = Array.isArray, integer = Number.isInteger;
        const TypeError = globalThis.TypeError;
        return function(query, options) {
            let limit = 10;
            if (arguments.length > 1) {
                if (options === null || typeof options !== 'object' || array(options))
                    throw new TypeError('searchTools options must be an object');
                const keys = names(options);
                if (symbols(options).length)
                    throw new TypeError('unknown searchTools option');
                for (let i = 0; i < keys.length; i++) {
                    if (keys[i] !== 'limit') throw new TypeError('unknown searchTools option');
                }
                if (keys.length) limit = options.limit;
                if (!integer(limit) || limit < 1 || limit > 10)
                    throw new TypeError('searchTools limit must be an integer 1..10');
            }
            return bridge(query, limit);
        };
    })"#,
    )?;
    let search: Function = search_wrapper.call((search,))?;
    ctx.globals().set("searchTools", search)?;
    ctx.globals().set("inspectSaved", inspect)?;
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

/// One worker-local clock shared with the VM interrupt. Only idle sleeps with
/// pending bridge replies earn time back; evaluation, reply resolution, getters,
/// serialization and jobs remain charged, even while a host call is pending.
struct GuestBudget {
    deadline: Cell<Instant>,
    exhausted: Cell<bool>,
}

impl GuestBudget {
    fn new(duration: Duration) -> Result<Self, String> {
        Ok(Self {
            deadline: Cell::new(
                Instant::now()
                    .checked_add(duration)
                    .ok_or("QuickJS execution budget overflow")?,
            ),
            exhausted: Cell::new(false),
        })
    }

    fn exhausted(&self) -> bool {
        if Instant::now() >= self.deadline.get() {
            self.exhausted.set(true);
        }
        self.exhausted.get()
    }

    fn wait_for_work(&self, host_pending: bool) -> Result<(), String> {
        if self.exhausted() {
            return Err("JavaScript execution budget exceeded".into());
        }
        let began = Instant::now();
        thread::sleep(Duration::from_millis(1));
        if host_pending {
            self.deadline.set(
                self.deadline
                    .get()
                    .checked_add(began.elapsed())
                    .ok_or("QuickJS execution budget overflow")?,
            );
        }
        Ok(())
    }
}

fn execute(
    code: String,
    tools_json: String,
    limits: CodeLimits,
    sender: mpsc::Sender<CodeRequest>,
    stop: &CancellationToken,
) -> CodeResult {
    let output = Rc::new(output::Output::new(limits.max_output_bytes, stop.clone()));
    let mut result = run_guest(code, tools_json, limits, sender, stop, &output);
    if output.exhausted() {
        result = Err("selected output byte limit exceeded".into());
    }
    CodeResult {
        result,
        output: output.take(),
    }
}

fn run_guest(
    code: String,
    tools_json: String,
    limits: CodeLimits,
    sender: mpsc::Sender<CodeRequest>,
    stop: &CancellationToken,
    output: &Rc<output::Output>,
) -> CodeReply {
    if stop.is_cancelled() {
        return Err("JavaScript cancelled".into());
    }
    // Worker startup time is not guest execution. The clock starts only once
    // this worker begins; cancellation still precedes VM construction.
    let budget = Rc::new(GuestBudget::new(limits.execution_budget)?);
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(limits.heap_bytes);
    runtime.set_max_stack_size(limits.stack_bytes);
    let interrupt_budget = budget.clone();
    let interrupt_stop = stop.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        interrupt_budget.exhausted() || interrupt_stop.is_cancelled()
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
        let helper = context
            .with(|ctx| diagnostic_helper(&ctx))
            .map_err(|e| context.with(|ctx| js_error(&ctx, e, None)))?;
        let root = context
            .with(|ctx| start_guest(ctx, &code, &tools_json, &bridge, output))
            .map_err(|e| context.with(|ctx| js_error(&ctx, e, Some(&helper))))?;
        loop {
            if stop.is_cancelled() {
                return Err("JavaScript cancelled".into());
            }
            if budget.exhausted() {
                return Err("JavaScript execution budget exceeded".into());
            }
            let result = context
                .with(|ctx| {
                    let root = root.clone().restore(&ctx)?;
                    // Observe rejection before settling any more bridge replies.
                    if let Some(value) = root.result::<Value>() {
                        let json = value.and_then(|v| {
                            let value = if v.is_undefined() {
                                Value::new_null(ctx.clone())
                            } else {
                                v
                            };
                            json_text(&ctx, value, limits.max_json_bytes)
                        })?;
                        output.finish(&ctx, json.len())?;
                        return Ok(Some(json));
                    }
                    poll_replies(&ctx, &bridge)?;
                    Ok(None)
                })
                .map_err(|e| context.with(|ctx| js_error(&ctx, e, Some(&helper))))?;
            if let Some(json) = result {
                return Ok(json);
            }
            // Outside Context::with: this API acquires the same runtime lock.
            match runtime.execute_pending_job() {
                Ok(true) => (),
                Ok(false) => {
                    let host_pending = !bridge.pending.borrow().is_empty();
                    budget.wait_for_work(host_pending)?;
                }
                Err(job) => {
                    return Err(job
                        .0
                        .with(|ctx| js_error(&ctx, rquickjs::Error::Exception, Some(&helper))));
                }
            }
        }
    }))
    .unwrap_or_else(|_| Err("QuickJS worker panicked".into()));
    let outcome = if stop.is_cancelled() {
        Err("JavaScript cancelled".into())
    } else if budget.exhausted() {
        Err("JavaScript execution budget exceeded".into())
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
            execution_budget: Duration::from_secs(2),
            max_json_bytes: 4096,
            ..CodeLimits::default()
        }
    }

    fn start(code: &str, limits: CodeLimits) -> (CodeTask, CancellationToken) {
        start_named(
            code,
            &[
                "forbidden",
                "echo",
                "effect",
                "host",
                "checkpoint",
                "waiting",
                "must not dispatch",
                "bad",
                "outer",
                "inner",
                "first",
                "next",
                "queued",
            ],
            limits,
        )
    }

    fn start_named(
        code: &str,
        names: &[&str],
        limits: CodeLimits,
    ) -> (CodeTask, CancellationToken) {
        let stop = CancellationToken::new();
        let task = QuickJs
            .start(
                code.into(),
                serde_json::to_string(names).unwrap(),
                limits,
                stop.clone(),
            )
            .unwrap();
        (task, stop)
    }

    async fn finish(task: &mut CodeTask) -> CodeReply {
        tokio::time::timeout(Duration::from_secs(3), &mut task.result)
            .await
            .expect("worker must terminate")
            .expect("worker must contain panics")
            .result
    }

    async fn request(task: &mut CodeTask) -> CodeRequest {
        tokio::time::timeout(Duration::from_secs(3), task.requests.recv())
            .await
            .expect("bridge must not block")
            .expect("request expected")
    }

    #[tokio::test]
    async fn checked_guest_stack_can_exceed_the_default_native_thread_stack() {
        let (mut task, stop) = start(
            "function recurse() { return 1 + recurse(); } return recurse();",
            CodeLimits {
                heap_bytes: 64 * 1024 * 1024,
                stack_bytes: 4 * 1024 * 1024,
                ..limits()
            },
        );
        let error = finish(&mut task).await.unwrap_err();
        assert!(error.to_ascii_lowercase().contains("stack"), "{error}");
        assert!(stop.is_cancelled());
        assert!(task.requests.recv().await.is_none());
        let (mut next, stop) = start("return 7;", limits());
        assert_eq!(finish(&mut next).await.unwrap(), "7");
        assert!(!stop.is_cancelled());
    }

    #[tokio::test]
    async fn useful_exception_messages_are_bounded() {
        for code in [
            "throw 'verification failed: expected exit 0';",
            "throw new Error('verification failed: expected exit 0');",
        ] {
            let (mut task, stop) = start(code, limits());
            assert_eq!(
                finish(&mut task).await,
                Err("verification failed: expected exit 0".into())
            );
            assert!(stop.is_cancelled());
            assert!(task.requests.recv().await.is_none());
        }
        for code in [
            "throw 'é🙂'.repeat(100000);",
            "throw new Error('é🙂'.repeat(100000));",
        ] {
            let (mut task, _) = start(code, limits());
            assert_eq!(
                finish(&mut task).await,
                Err(format!("{}é", "é🙂".repeat(42)))
            );
            assert!(task.requests.recv().await.is_none());
        }
        for (code, expected) in [
            ("throw new Error('a'.repeat(255)+'🦀');", "a".repeat(255)),
            ("throw '';", "JavaScript exception".into()),
        ] {
            let (mut task, stop) = start(code, limits());
            assert_eq!(finish(&mut task).await, Err(expected));
            assert!(stop.is_cancelled());
            assert!(task.requests.recv().await.is_none());
        }
        let (mut task, _) = start("return (;", limits());
        let error = finish(&mut task).await.unwrap_err();
        assert_ne!(error, "JavaScript exception");
        assert!(error.len() <= 256);
        assert!(task.requests.recv().await.is_none());
    }

    #[test]
    fn diagnostics_reject_proxies_without_traps() {
        // Proxy is not installed in the production VM. Exercise the native
        // class gate in a context that does provide it, without widening it.
        for expression in [
            "new Proxy(new Error('hidden'), {get: hook, getOwnPropertyDescriptor: hook, getPrototypeOf: hook})",
            "(() => {const p = Proxy.revocable(new Error('hidden'), {}); p.revoke(); return p.proxy;})()",
        ] {
            let runtime = Runtime::new().unwrap();
            let context = Context::custom::<(intrinsic::Eval, intrinsic::Proxy)>(&runtime).unwrap();
            context.with(|ctx| {
                let helper = diagnostic_helper(&ctx).unwrap();
                let error = ctx.eval::<Value, _>(format!("let touched = false; const hook = () => {{touched = true;}}; throw {expression};")).unwrap_err();
                assert_eq!(js_error(&ctx, error, Some(&helper)), "JavaScript exception");
                assert!(!ctx.eval::<bool, _>("touched").unwrap());
            });
        }
    }

    #[tokio::test]
    async fn diagnostics_do_not_execute_guest_hooks_or_mutated_globals() {
        for expression in [
            "Object.defineProperty(new Error(), 'message', {get: hook})",
            "Object.defineProperty(new Error(), 'message', {value: {toString: hook, [Symbol.toPrimitive]: hook}})",
            "({get message() {hook()}, toString: hook, [Symbol.toPrimitive]: hook})",
        ] {
            let code = format!(
                "const hook = () => {{void tools.forbidden({{}}); return 'unsafe';}}; throw {expression};"
            );
            let (mut task, stop) = start(&code, limits());
            assert_eq!(
                finish(&mut task).await,
                Err("JavaScript exception".into()),
                "{expression}"
            );
            assert!(stop.is_cancelled());
            assert!(task.requests.recv().await.is_none(), "{expression}");
        }
        let (mut task, _) = start(
            "const e = new Error('safe'); const hook = () => {void tools.forbidden({}); return 'unsafe';}; Object.getOwnPropertyDescriptor = hook; String.prototype.slice = hook; Function.prototype.call = hook; Function.prototype.bind = hook; Object.defineProperty(e, 'stack', {get: hook}); Object.defineProperty(globalThis, 'String', {get: hook}); Object.defineProperty(globalThis, 'Object', {get: hook}); throw e;",
            limits(),
        );
        assert_eq!(finish(&mut task).await, Err("safe".into()));
        assert!(task.requests.recv().await.is_none());
    }

    #[tokio::test]
    async fn awaited_json_calls_discovery_and_inspect() {
        let (mut task, stop) = start(
            "const a = await tools.echo({n: 1}); const b = await describeTool('ec'); const c = await inspectSaved({kind:'children',parent:{assistant_entry:12,ordinal:0}}); return {a, b, c};",
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
        assert!(matches!(describe.kind, CodeRequestKind::DescribeTool { name } if name == "ec"));
        describe
            .reply
            .send(Ok(
                json!({"name":"ec","description":"Echo","input_schema":{"type":"object"},"result_schema":null})
                    .to_string(),
            ))
            .unwrap();
        let inspect = request(&mut task).await;
        assert!(
            matches!(inspect.kind, CodeRequestKind::Inspect { query_json }
            if serde_json::from_str::<serde_json::Value>(&query_json).unwrap() == json!({"kind":"children","parent":{"assistant_entry":12,"ordinal":0}}))
        );
        inspect.reply.send(Ok("{\"children\":[]}".into())).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&finish(&mut task).await.unwrap()).unwrap(),
            json!({"a": {"value": 2, "is_error": false}, "b": {"name":"ec","description":"Echo","input_schema":{"type":"object"},"result_schema":null}, "c":{"children":[]}}),
        );
        assert!(!stop.is_cancelled());
        assert!(task.requests.recv().await.is_none());
    }

    #[tokio::test]
    async fn discovery_types_and_shared_limits_reject_without_dispatch() {
        for helper in ["describeTool", "searchTools"] {
            for arg in [
                "undefined",
                "null",
                "12",
                "{}",
                "[]",
                "true",
                "'x'.repeat(5000)",
            ] {
                let (mut task, stop) = start(&format!("return await {helper}({arg});"), limits());
                assert!(finish(&mut task).await.is_err(), "{helper}({arg})");
                assert!(stop.is_cancelled());
                assert!(task.requests.recv().await.is_none());
            }
        }
        let (mut task, stop) = start(
            "await searchTools('candidate'); await describeTool('candidate'); await tools['must not dispatch']({});",
            CodeLimits {
                max_calls: 2,
                max_concurrency: 1,
                ..limits()
            },
        );
        for reply in ["[]", "null"] {
            request(&mut task)
                .await
                .reply
                .send(Ok(reply.into()))
                .unwrap();
        }
        assert!(finish(&mut task).await.is_err());
        assert!(stop.is_cancelled());
        assert!(task.requests.recv().await.is_none());
    }

    #[tokio::test]
    async fn search_options_validate_before_admission_and_preserve_defaults() {
        let (mut task, stop) = start(
            r#"
            const invalid = [undefined, null, false, 2, 'x', [], () => {},
                {extra:1}, {limit:null}, {limit:undefined}, {limit:'1'}, {limit:true},
                {limit:0}, {limit:11}, {limit:-1}, {limit:1.5}, {limit:NaN}, {limit:Infinity},
                {[Symbol('extra')]:1}, Object.defineProperty({}, 'extra', {value:1})];
            for (const options of invalid) {
                let rejected = false;
                try { await searchTools('target', options); } catch (_) { rejected = true; }
                if (!rejected) throw new Error('invalid options admitted');
            }
            Object.getOwnPropertyNames = () => [];
            Number.isInteger = () => true;
            try { await searchTools('target', {limit:null}); throw new Error('coerced'); }
            catch (e) { if (e.message === 'coerced') throw e; }
            return await searchTools('target', {limit:1});
        "#,
            CodeLimits {
                max_calls: 1,
                max_concurrency: 1,
                ..limits()
            },
        );
        let query = request(&mut task).await;
        assert!(
            matches!(query.kind, CodeRequestKind::SearchTools { query, limit:1 } if query == "target")
        );
        query.reply.send(Ok("[]".into())).unwrap();
        assert_eq!(finish(&mut task).await, Ok("[]".into()));
        assert!(!stop.is_cancelled());
        assert!(task.requests.recv().await.is_none());
        for options in ["", ", {}", ", {limit:10}"] {
            let (mut task, _) = start(
                &format!("return await searchTools('target'{options});"),
                limits(),
            );
            let query = request(&mut task).await;
            assert!(matches!(
                query.kind,
                CodeRequestKind::SearchTools { limit: 10, .. }
            ));
            query.reply.send(Ok("[]".into())).unwrap();
            assert_eq!(finish(&mut task).await, Ok("[]".into()));
        }
    }

    #[tokio::test]
    async fn unawaited_transfer_survives_successful_return() {
        let (mut task, stop) = start("void tools.effect({n: 1}); return 7;", limits());
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
    async fn awaited_host_latency_does_not_exhaust_guest_budget() {
        for reply in [
            json!({"value": 7, "is_error": false}),
            json!({"value": {"exit_code": 7}, "is_error": true}),
        ] {
            let (mut task, stop) = start(
                "return await tools.host({});",
                CodeLimits {
                    execution_budget: Duration::from_millis(100),
                    ..limits()
                },
            );
            let call = request(&mut task).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            let delivered = call.reply.send(Ok(reply.to_string())).is_ok();
            assert_eq!(finish(&mut task).await, Ok(reply.to_string()));
            assert!(delivered);
            assert!(!stop.is_cancelled());
            assert!(task.requests.recv().await.is_none());
        }
    }

    #[tokio::test]
    async fn reply_guest_code_and_idle_promises_resume_budget_accounting() {
        for code in [
            "await tools.host({}); while(true) {}",
            "await describeTool('host'); while(true) {}",
            "await searchTools('host'); while(true) {}",
            "await tools.host({}); await new Promise(() => {});",
            "Object.defineProperty(Object.prototype, 'then', {get() {while(true) {}}}); return await tools.host({});",
            "await tools.host({}); return {toJSON() {while(true) {}}};",
        ] {
            let (mut task, stop) = start(
                code,
                CodeLimits {
                    execution_budget: Duration::from_millis(100),
                    ..limits()
                },
            );
            let call = request(&mut task).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            let delivered = call.reply.send(Ok("{}".into())).is_ok();
            assert_eq!(
                finish(&mut task).await,
                Err("JavaScript execution budget exceeded".into()),
                "{code}"
            );
            assert!(delivered, "host wait consumed the budget: {code}");
            assert!(stop.is_cancelled());
            assert!(task.requests.recv().await.is_none());
        }
    }

    #[tokio::test]
    async fn host_replies_do_not_reset_accumulated_execution_budget() {
        let (mut task, stop) = start(
            "for(let i=0;i<512;i++) { for(let j=0;j<1000000;j++) {} await tools.checkpoint({}); } return 'budget reset';",
            CodeLimits {
                execution_budget: Duration::from_millis(100),
                max_calls: 1024,
                ..limits()
            },
        );
        let mut calls = 0;
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                tokio::select! {
                    biased;
                    result = &mut task.result => break result.unwrap(),
                    call = task.requests.recv() => {
                        let Some(call) = call else { break (&mut task.result).await.unwrap(); };
                        if calls < 2 {
                            tokio::time::sleep(Duration::from_millis(150)).await;
                        }
                        calls += 1;
                        let _ = call.reply.send(Ok("null".into()));
                    }
                }
            }
        })
        .await;
        if result.is_err() {
            stop.cancel();
            let _ = finish(&mut task).await;
        }
        assert_eq!(
            result.unwrap().result,
            Err("JavaScript execution budget exceeded".into())
        );
        assert!(stop.is_cancelled());
        assert!(task.requests.recv().await.is_none());
    }

    #[tokio::test]
    async fn execution_budget_bounds_cpu_idle_promises_and_jobs() {
        for code in [
            "void tools.waiting({}); while (true) {}",
            "void tools.waiting({}); const again = () => Promise.resolve().then(again); again(); await new Promise(() => {});",
            "while (true) {}",
            "await Promise.resolve(); while (true) {}",
            "await new Promise(() => {});",
            "const again = () => Promise.resolve().then(again); again(); await new Promise(() => {});",
        ] {
            let (mut task, stop) = start(
                code,
                CodeLimits {
                    execution_budget: Duration::from_millis(30),
                    ..limits()
                },
            );
            let began = Instant::now();
            assert_eq!(
                finish(&mut task).await,
                Err("JavaScript execution budget exceeded".into()),
                "{code}"
            );
            assert!(began.elapsed() < Duration::from_secs(1), "{code}");
            assert!(stop.is_cancelled());
            if code.contains("tools.waiting(") {
                let call = request(&mut task).await;
                assert!(
                    matches!(call.kind, CodeRequestKind::Call { name, .. } if name == "waiting")
                );
            }
            assert!(task.requests.recv().await.is_none());
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_awaited_host_and_bare_promise() {
        let stop = CancellationToken::new();
        stop.cancel();
        let mut task = QuickJs
            .start(
                "void tools['must not transfer']({}); return 0;".into(),
                "[\"must not transfer\"]".into(),
                limits(),
                stop,
            )
            .unwrap();
        assert_eq!(finish(&mut task).await, Err("JavaScript cancelled".into()));
        assert!(task.requests.recv().await.is_none());

        for code in ["await tools.waiting({});", "await new Promise(() => {});"] {
            let (mut task, stop) = start(code, limits());
            let held = if code.contains("tools.waiting(") {
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
            "const a = {}; a.a = a; return a;",
            "return 1n;",
            "return 'x'.repeat(5000);",
            "await tools.bad('x'.repeat(5000));",
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
    async fn selected_output_snapshots_values_and_survives_script_failure() {
        for fails in [false, true] {
            let body = format!(
                "text({{toJSON() {{ text('inner'); return {{n:2}}; }} }}); image({{type:'image',id:'current'}}); {}",
                if fails {
                    "throw new Error('after selection');"
                } else {
                    ""
                }
            );
            let (mut task, stop) = start(&body, limits());
            let result = tokio::time::timeout(Duration::from_secs(3), &mut task.result)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(result.output).unwrap(),
                json!([
                    {"type":"text", "text":"inner"},
                    {"type":"text", "text":"{\"n\":2}"},
                    {"type":"image", "id":"current"}
                ])
            );
            if fails {
                assert_eq!(result.result.unwrap_err(), "after selection");
            } else {
                assert_eq!(result.result.unwrap(), "null");
            }
            assert_eq!(stop.is_cancelled(), fails);
        }
    }

    #[tokio::test]
    async fn implicit_and_explicit_null_returns_share_the_json_bound() {
        for body in ["", "return null;"] {
            for limit in [3, 4] {
                let (mut task, _) = start_named(
                    body,
                    &[],
                    CodeLimits {
                        max_json_bytes: limit,
                        ..limits()
                    },
                );
                let result = finish(&mut task).await;
                if limit == 3 {
                    assert!(result.unwrap_err().contains("JSON byte limit"));
                } else {
                    assert_eq!(result.unwrap(), "null");
                }
            }
        }
    }

    #[tokio::test]
    async fn selected_output_limit_is_cumulative_and_cannot_be_caught() {
        for body in [
            "try { text('x'.repeat(100)); } catch (_) {} return true;",
            "text('x'.repeat(40)); return 'x'.repeat(40);",
        ] {
            let (mut task, stop) = start(
                body,
                CodeLimits {
                    max_output_bytes: 100,
                    ..limits()
                },
            );
            assert!(
                finish(&mut task)
                    .await
                    .unwrap_err()
                    .contains("selected output byte limit")
            );
            assert!(stop.is_cancelled());
        }
    }

    #[tokio::test]
    async fn named_namespace_is_exact_immutable_and_has_no_ambient_authority() {
        let names = [
            "call",
            "inspect",
            "__proto__",
            "constructor",
            "a-b",
            "a_b",
            "Read",
            "read",
        ];
        let (mut task, stop) = start_named(
            "const absent = ['process','require','module','exports','fs','fetch','XMLHttpRequest','WebSocket',
                'setTimeout','setInterval','setImmediate','queueMicrotask','console','Deno','Bun','std','os'];
             const names = Object.keys(tools);
             for (const name of names) await tools[name]({name});
             return {absent: absent.every(k => typeof globalThis[k] === 'undefined'),
                     names, frozen: Object.isFrozen(tools), nullPrototype: Object.getPrototypeOf(tools) === null,
                     unknown: tools.unknown === undefined, savedInspection: typeof inspectSaved === 'function',
                     immutable: !Reflect.set(tools, 'read', () => {})};",
            &names,
            limits(),
        );
        for expected in names {
            let call = request(&mut task).await;
            assert!(
                matches!(call.kind, CodeRequestKind::Call { name, args_json }
                if name == expected && serde_json::from_str::<serde_json::Value>(&args_json).unwrap() == json!({"name":expected}))
            );
            call.reply.send(Ok("null".into())).unwrap();
        }
        let result: serde_json::Value =
            serde_json::from_str(&finish(&mut task).await.unwrap()).unwrap();
        assert_eq!(
            result,
            json!({"absent":true,"names":names,"frozen":true,"nullPrototype":true,
            "unknown":true,"savedInspection":true,"immutable":true})
        );
        assert!(task.requests.recv().await.is_none());
        assert!(!stop.is_cancelled());
    }

    #[tokio::test]
    async fn serialization_reentry_cannot_exceed_admission() {
        let (mut task, stop) = start(
            "try { await tools.outer({toJSON() { void tools.inner({}); return {}; }}); }
             catch (e) { return e.message; }",
            CodeLimits {
                max_calls: 1,
                max_concurrency: 1,
                ..limits()
            },
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
                 if (!fired) { fired = true; void searchTools('nested'); } return undefined;
             }});
             const a = await tools.first({}); delete Object.prototype.then; return a;",
            limits(),
        );
        let call = request(&mut task).await;
        call.reply.send(Ok("{\"n\":1}".into())).unwrap();
        assert_eq!(finish(&mut task).await, Ok("{\"n\":1}".into()));
        let nested = request(&mut task).await;
        assert!(
            matches!(nested.kind, CodeRequestKind::SearchTools { query, limit: 10 } if query == "nested")
        );
        assert!(task.requests.recv().await.is_none());
        assert!(!stop.is_cancelled());
    }

    #[tokio::test]
    async fn total_calls_are_bounded_even_after_replies_settle() {
        let (mut task, stop) = start(
            "for (let i = 0; i < 3; i++) await tools.next({i}); return true;",
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
            "void tools.queued({}); throw new Error('failed');",
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
            let (mut task, stop) = start("return await tools.host({});", limits());
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

    #[test]
    fn invalid_manifests_fail_before_worker_creation() {
        for (names, limits, message) in [
            ("null", limits(), "array of names"),
            ("[1]", limits(), "array of names"),
            ("[\"read\",\"read\"]", limits(), "duplicate names"),
            ("[\"then\"]", limits(), "reserved tool name"),
            ("[\"toJSON\"]", limits(), "reserved tool name"),
            (
                "[]",
                CodeLimits {
                    max_json_bytes: 1,
                    ..limits()
                },
                "manifest byte limit",
            ),
            (
                "[]",
                CodeLimits {
                    max_host_bytes: 1,
                    ..limits()
                },
                "manifest byte limit",
            ),
        ] {
            let stop = CancellationToken::new();
            let error = QuickJs
                .start("return 0;".into(), names.into(), limits, stop.clone())
                .err()
                .expect("invalid manifest started a worker");
            assert!(error.contains(message), "{error}");
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
                execution_budget: Duration::ZERO,
                ..base
            },
            CodeLimits {
                execution_budget: Duration::MAX,
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
                    .start("return 0;".into(), "[]".into(), limits, stop.clone())
                    .is_err(),
                "{limits:?}"
            );
            assert!(stop.is_cancelled());
        }
    }
}
