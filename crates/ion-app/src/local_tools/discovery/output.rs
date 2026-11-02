//! Bounded JSON output and previews shared by discovery tools.
use std::path::Path;

use serde_json::{Value, json};

pub(super) const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const RESERVED_METADATA_BYTES: usize = 16 * 1024;
const MAX_DIAGNOSTICS: usize = 4;
const PREVIEW_BYTES: usize = 512;

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

pub(super) fn preview(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(3);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = text[..end].to_owned();
    text.push_str("...");
    text
}

pub(super) struct OutputBudget {
    values: Vec<Value>,
    used: usize,
    cap: usize,
    limited: bool,
}

impl OutputBudget {
    /// Reserve room for metadata, diagnostics and omission notices. Item cost is
    /// the actual serialized item plus its list separator, not raw snippet size.
    pub(super) fn new(base: &Value) -> Self {
        Self {
            values: Vec::new(),
            used: serialized_len(base),
            cap: MAX_OUTPUT_BYTES - RESERVED_METADATA_BYTES,
            limited: false,
        }
    }

    pub(super) fn push(&mut self, item: Value) -> bool {
        let cost = serde_json::to_vec(&item)
            .map(|bytes| bytes.len().saturating_add(1))
            .unwrap_or(usize::MAX);
        if self.used.saturating_add(cost) > self.cap {
            self.limited = true;
            return false;
        }
        self.used += cost;
        self.values.push(item);
        true
    }

    pub(super) fn finish(self) -> (Vec<Value>, bool) {
        (self.values, self.limited)
    }
}

/// Drop trailing items if final metadata still exceeds the serialized bound. The
/// conservative item budget normally prevents this path.
pub(super) fn finalize(mut value: Value, item_key: &str, omission: &str) -> Value {
    let mut dropped = false;
    loop {
        if serialized_len(&value) <= MAX_OUTPUT_BYTES {
            break;
        }
        let popped = value
            .get_mut(item_key)
            .and_then(Value::as_array_mut)
            .is_some_and(|items| items.pop().is_some());
        if !popped {
            break;
        }
        dropped = true;
        update_item_metadata(&mut value, item_key);
    }
    if dropped {
        value["output_limited"] = json!(true);
        if let Some(omissions) = value["omissions"].as_array_mut() {
            omissions.push(json!(omission));
        }
        while serialized_len(&value) > MAX_OUTPUT_BYTES {
            let popped = value
                .get_mut(item_key)
                .and_then(Value::as_array_mut)
                .is_some_and(|items| items.pop().is_some());
            if !popped {
                break;
            }
            update_item_metadata(&mut value, item_key);
        }
    }
    debug_assert!(
        serialized_len(&value) <= MAX_OUTPUT_BYTES,
        "discovery metadata reserve was insufficient"
    );
    value
}

fn update_item_metadata(value: &mut Value, item_key: &str) {
    let returned = value
        .get(item_key)
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    value["returned"] = json!(returned);
    value["exhaustive"] = json!(false);
}

#[derive(Default)]
pub(super) struct Diagnostics {
    entries: Vec<Value>,
    omitted: usize,
}

impl Diagnostics {
    pub(super) fn push(&mut self, path: &Path, message: impl std::fmt::Display) {
        if self.entries.len() >= MAX_DIAGNOSTICS {
            self.omitted += 1;
            return;
        }
        self.entries.push(json!({
            "path": preview(&path.to_string_lossy(), PREVIEW_BYTES),
            "error": preview(&message.to_string(), PREVIEW_BYTES),
        }));
    }

    pub(super) fn omitted(&self) -> usize {
        self.omitted
    }

    pub(super) fn finish(self) -> Vec<Value> {
        self.entries
    }
}
