//! Advisory authoring types, not a JSON Schema validator or a value converter.
use std::io::{self, Write};

use ion_ai::ToolSpec;
use serde_json::Value;

const MAX_TYPE_BYTES: usize = 16_000;
const MAX_NODES: usize = 512;
const MAX_DEPTH: usize = 32;

#[cfg(test)]
mod tests;

pub(super) fn render(spec: &ToolSpec) -> String {
    let mut writer = TypeWriter {
        root: &spec.input_schema,
        output: TypeOutput(String::new()),
        remaining: MAX_NODES,
    };
    let limited = writer.schema(&spec.input_schema, 0).is_err();
    let (notice, input) = if limited {
        (
            "// Input type exceeds declaration limits; consult input_schema.\n",
            "unknown",
        )
    } else {
        ("", writer.output.0.as_str())
    };
    // Names are never shortened or normalized. The complete declaration is still
    // charged to the gateway's per-value and cumulative host reply budgets.
    let name = serde_json::to_string(&spec.name).expect("tool names are JSON strings");
    let name = name
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!(
        "// Advisory types, not validation; consult input_schema and result_schema.\n\
         {notice}declare const tools: {{\n  {name}(args: {input}): Promise<{{ value: unknown; is_error: boolean; images: Array<{{ type: \"image\"; id: string; mime_type: string }}>; }}>;\n}};"
    )
}

struct TypeOutput(String);
impl TypeOutput {
    fn push(&mut self, text: &str) -> Result<(), ()> {
        if text.len() > MAX_TYPE_BYTES - self.0.len() {
            return Err(());
        }
        for ch in text.chars() {
            let escaped = match ch {
                '\u{2028}' => Some("\\u2028"),
                '\u{2029}' => Some("\\u2029"),
                _ => None,
            };
            let bytes = escaped.map_or(ch.len_utf8(), str::len);
            if bytes > MAX_TYPE_BYTES - self.0.len() {
                return Err(());
            }
            if let Some(escaped) = escaped {
                self.0.push_str(escaped);
            } else {
                self.0.push(ch);
            }
        }
        Ok(())
    }
    fn quoted(&mut self, text: &str) -> Result<(), ()> {
        // serde_json scans an escape-free string before writing it. Reject
        // oversized source strings before that scan, not just at the sink.
        if text.len() > MAX_TYPE_BYTES - self.0.len() {
            return Err(());
        }
        serde_json::to_writer(self, text).map_err(|_| ())
    }
}
impl Write for TypeOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
        self.push(text)
            .map_err(|()| io::Error::other("declaration type limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct TypeWriter<'a> {
    root: &'a Value,
    output: TypeOutput,
    remaining: usize,
}
impl<'a> TypeWriter<'a> {
    fn visit(&mut self, depth: usize) -> Result<(), ()> {
        if depth > MAX_DEPTH || self.remaining == 0 {
            return Err(());
        }
        self.remaining -= 1;
        Ok(())
    }
    fn schema(&mut self, schema: &'a Value, depth: usize) -> Result<(), ()> {
        self.visit(depth)?;
        // Unsupported dialects can give familiar keywords different meanings
        // (e.g. draft-07 ignores prefixItems). Don't infer narrower arguments.
        if unsupported_dialect(schema) {
            return self.output.push("unknown");
        }
        // Nested schema resources change reference scope. Without a resource
        // registry, unknown is safer than resolving against the wrong root.
        if depth != 0 && has_resource_id(schema) {
            return self.output.push("unknown");
        }
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            if let Some(pointer) = local_pointer(reference)?
                && let Some(target) = local_target(self.root, &pointer)
            {
                return self.schema(target, depth + 1);
            }
            return self.output.push("unknown");
        }
        if let Some(value) = schema.get("const") {
            return self.literal(value, depth + 1);
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            if values.is_empty() {
                return self.output.push("never");
            }
            self.output.push("(")?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    self.output.push(" | ")?;
                }
                self.literal(value, depth + 1)?;
            }
            return self.output.push(")");
        }
        for (keyword, separator) in [("anyOf", " | "), ("oneOf", " | "), ("allOf", " & ")] {
            if let Some(variants) = schema.get(keyword).and_then(Value::as_array) {
                self.output.push("(")?;
                if variants.is_empty() {
                    self.output.push(if keyword == "allOf" {
                        "unknown"
                    } else {
                        "never"
                    })?;
                }
                for (index, variant) in variants.iter().enumerate() {
                    if index != 0 {
                        self.output.push(separator)?;
                    }
                    self.schema(variant, depth + 1)?;
                }
                return self.output.push(")");
            }
        }
        if let Some(types) = schema.get("type").and_then(Value::as_array) {
            self.output.push("(")?;
            if types.is_empty() {
                self.output.push("unknown")?;
            }
            for (index, kind) in types.iter().enumerate() {
                self.visit(depth + 1)?;
                if index != 0 {
                    self.output.push(" | ")?;
                }
                self.kind(schema, kind.as_str(), depth)?;
            }
            return self.output.push(")");
        }
        if schema == &Value::Bool(false) {
            return self.output.push("never");
        }
        self.kind(schema, schema.get("type").and_then(Value::as_str), depth)
    }
    fn kind(&mut self, schema: &'a Value, kind: Option<&str>, depth: usize) -> Result<(), ()> {
        match kind {
            Some("string") => self.output.push("string"),
            Some("number" | "integer") => self.output.push("number"),
            Some("boolean") => self.output.push("boolean"),
            Some("null") => self.output.push("null"),
            Some("object") => self.object(schema, depth + 1),
            Some("array") => self.array(schema, depth + 1),
            // Object/array keywords without `type` also accept other JSON types.
            _ => self.output.push("unknown"),
        }
    }
    fn object(&mut self, schema: &'a Value, depth: usize) -> Result<(), ()> {
        let properties = schema.get("properties").and_then(Value::as_object);
        let required = schema.get("required").and_then(Value::as_array);
        if properties.is_some_and(|properties| properties.len() > MAX_NODES)
            || required.is_some_and(|keys| keys.len() > MAX_NODES)
        {
            return Err(());
        }
        // Pattern keys are not expressible as a TS index signature; don't rule
        // out valid matching keys when additionalProperties is false.
        let additional = if schema
            .get("patternProperties")
            .and_then(Value::as_object)
            .is_some_and(|patterns| !patterns.is_empty())
        {
            None
        } else {
            schema.get("additionalProperties")
        };
        if properties.is_none_or(serde_json::Map::is_empty)
            && additional == Some(&Value::Bool(false))
        {
            return self.output.push("Record<string, never>");
        }
        self.output.push("{ ")?;
        if let Some(properties) = properties {
            for (name, property) in properties {
                self.output.quoted(name)?;
                if !required.is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some(name))) {
                    self.output.push("?")?;
                }
                self.output.push(": ")?;
                self.schema(property, depth)?;
                self.output.push("; ")?;
            }
        }
        match additional {
            Some(Value::Bool(false)) => {}
            None | Some(Value::Bool(true)) => self.output.push("[key: string]: unknown; ")?,
            Some(additional) => {
                // TS index signatures also cover named properties, unlike JSON
                // Schema's additionalProperties. Widen rather than emit an
                // incompatible or uninhabitable type for otherwise valid data.
                self.output.push("[key: string]: (")?;
                self.schema(additional, depth)?;
                if let Some(properties) = properties {
                    for property in properties.values() {
                        self.output.push(" | ")?;
                        self.schema(property, depth)?;
                    }
                    if properties.keys().any(|name| {
                        !required
                            .is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some(name)))
                    }) {
                        self.output.push(" | undefined")?;
                    }
                }
                self.output.push("); ")?;
            }
        }
        self.output.push("}")
    }
    fn array(&mut self, schema: &'a Value, depth: usize) -> Result<(), ()> {
        let legacy = schema.get("items").and_then(Value::as_array);
        let prefix = schema
            .get("prefixItems")
            .and_then(Value::as_array)
            .or(legacy);
        if let Some(prefix) = prefix {
            self.output.push("[")?;
            for item in prefix {
                self.output.push("(")?;
                self.schema(item, depth)?;
                // Prefix schemas do not themselves require that index to exist.
                self.output.push(")?, ")?;
            }
            let rest = schema.get(if legacy.is_some() {
                "additionalItems"
            } else {
                "items"
            });
            if rest != Some(&Value::Bool(false)) {
                self.output.push("...Array<")?;
                if let Some(rest) = rest {
                    self.schema(rest, depth)?;
                } else {
                    self.output.push("unknown")?;
                }
                self.output.push(">")?;
            }
            return self.output.push("]");
        }
        self.output.push("Array<")?;
        if let Some(item) = schema.get("items") {
            self.schema(item, depth)?;
        } else {
            self.output.push("unknown")?;
        }
        self.output.push(">")
    }
    fn literal(&mut self, value: &Value, depth: usize) -> Result<(), ()> {
        self.visit(depth)?;
        match value {
            Value::String(text) => self.output.quoted(text),
            Value::Array(items) => {
                self.output.push("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index != 0 {
                        self.output.push(", ")?;
                    }
                    self.literal(item, depth + 1)?;
                }
                self.output.push("]")
            }
            Value::Object(properties) => {
                self.output.push("{ ")?;
                for (name, property) in properties {
                    self.output.quoted(name)?;
                    self.output.push(": ")?;
                    self.literal(property, depth + 1)?;
                    self.output.push("; ")?;
                }
                self.output.push("}")
            }
            _ => self.output.push(&value.to_string()),
        }
    }
}

fn unsupported_dialect(schema: &Value) -> bool {
    schema
        .get("$schema")
        .and_then(Value::as_str)
        .is_some_and(|dialect| {
            dialect.strip_suffix('#').unwrap_or(dialect)
                != "https://json-schema.org/draft/2020-12/schema"
        })
}

fn has_resource_id(schema: &Value) -> bool {
    // `id` covers legacy schema resources too; no dialect-specific resolver is
    // implied by these authoring hints.
    ["$id", "id"]
        .iter()
        .any(|key| schema.get(key).is_some_and(Value::is_string))
}

fn local_target<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for segment in pointer.strip_prefix('/')?.split('/') {
        current = current.pointer(&format!("/{segment}"))?;
        // A direct pointer into a nested resource must not skip its scope.
        if has_resource_id(current) || unsupported_dialect(current) {
            return None;
        }
    }
    Some(current)
}

// A local URI fragment is percent-decoded before JSON Pointer handles ~0/~1.
// Remote references remain unknown; this projection never performs I/O.
fn local_pointer(reference: &str) -> Result<Option<String>, ()> {
    let Some(fragment) = reference.strip_prefix('#') else {
        return Ok(None);
    };
    if fragment.len() > MAX_TYPE_BYTES {
        return Err(());
    }
    let mut bytes = Vec::with_capacity(fragment.len());
    let mut source = fragment.bytes();
    while let Some(byte) = source.next() {
        if byte == b'%' {
            let Some(hi) = source.next().and_then(|b| (b as char).to_digit(16)) else {
                return Ok(None);
            };
            let Some(lo) = source.next().and_then(|b| (b as char).to_digit(16)) else {
                return Ok(None);
            };
            bytes.push((hi * 16 + lo) as u8);
        } else {
            bytes.push(byte);
        }
    }
    Ok(String::from_utf8(bytes).ok())
}
