//! Explicit, bounded selections. Image bytes remain in Core's observed results.
use std::{
    cell::{Cell, RefCell},
    io::{self, Write},
    rc::Rc,
};

use ion_core::CodeOutput;
use rquickjs::{Ctx, Exception, Function, Object};
use tokio_util::sync::CancellationToken;

struct Size(usize);
impl Write for Size {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) struct Output {
    items: RefCell<Vec<CodeOutput>>,
    bytes: Cell<usize>,
    limit: usize,
    exhausted: Cell<bool>,
    stop: CancellationToken,
}

impl Output {
    pub(super) fn new(limit: usize, stop: CancellationToken) -> Self {
        Self {
            items: RefCell::new(Vec::new()),
            bytes: Cell::new(2),
            limit,
            exhausted: Cell::new(false),
            stop,
        }
    }

    fn charge(&self, ctx: &Ctx<'_>, bytes: usize) -> rquickjs::Result<()> {
        if self.exhausted.get() || bytes > self.limit.saturating_sub(self.bytes.get()) {
            self.exhausted.set(true);
            // A caught RangeError must not admit more effects after exhaustion.
            self.stop.cancel();
            return Err(Exception::throw_range(
                ctx,
                "selected output byte limit exceeded",
            ));
        }
        self.bytes.set(self.bytes.get() + bytes);
        Ok(())
    }

    fn push(&self, ctx: &Ctx<'_>, item: CodeOutput) -> rquickjs::Result<()> {
        // No guest code runs while the buffer is borrowed. Each item is owned
        // text/metadata; serializing it cannot invoke a guest getter or toJSON.
        let mut size = Size(0);
        serde_json::to_writer(&mut size, &item).expect("text and image IDs serialize");
        self.charge(
            ctx,
            size.0
                .saturating_add(usize::from(!self.items.borrow().is_empty())),
        )?;
        self.items.borrow_mut().push(item);
        Ok(())
    }

    pub(super) fn finish(&self, ctx: &Ctx<'_>, return_bytes: usize) -> rquickjs::Result<()> {
        self.charge(ctx, return_bytes)
    }

    pub(super) fn exhausted(&self) -> bool {
        self.exhausted.get()
    }

    pub(super) fn take(&self) -> Vec<CodeOutput> {
        self.items.take()
    }

    pub(super) fn install<'js>(self: &Rc<Self>, ctx: &Ctx<'js>) -> rquickjs::Result<()> {
        let output = self.clone();
        let text = Function::new(ctx.clone(), move |ctx: Ctx<'js>, text: String| {
            output.push(&ctx, CodeOutput::Text { text })
        })?;
        let output = self.clone();
        let image = Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: String| {
            output.push(&ctx, CodeOutput::Image { id })
        })?;
        // Capture builtins before user code; coercion/getters run before either
        // native append and may safely reenter the helpers or tool bridge.
        let factory: Function = ctx.eval(r#"((appendText, appendImage) => {
            const stringify = JSON.stringify, string = String;
            const TypeError = globalThis.TypeError, array = Array.isArray;
            return {
                text(value) {
                    let rendered;
                    if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
                        rendered = string(value);
                    } else {
                        rendered = stringify(value);
                        if (rendered === undefined) rendered = string(value);
                    }
                    appendText(rendered);
                },
                image(block) {
                    if (!block || typeof block !== 'object' || array(block) || block.type !== 'image')
                        throw new TypeError('image expects an image block returned by a tool in this invocation');
                    const id = block.id;
                    if (typeof id !== 'string' || !id.length)
                        throw new TypeError('image block has no invocation-scoped ID');
                    appendImage(id);
                }
            };
        })"#)?;
        let helpers: Object = factory.call((text, image))?;
        ctx.globals()
            .set("text", helpers.get::<_, Function>("text")?)?;
        ctx.globals()
            .set("image", helpers.get::<_, Function>("image")?)?;
        Ok(())
    }
}
