from pathlib import Path

path = Path("crates/ion-core/src/provider.rs")
text = path.read_text()
anchor = '''pub struct TokenUsage {\n    /// Fresh input tokens, excluding the cache-read/write buckets below.\n    /// The four fields are disjoint and may be summed for context accounting.\n    pub input: u64,\n    pub output: u64,\n    /// Tokens served from the provider prompt cache (§14.4).\n    pub cache_read: u64,\n    /// Tokens written to the provider prompt cache (§14.4).\n    pub cache_write: u64,\n}\n'''
replacement = anchor + '''\nimpl TokenUsage {\n    /// Total tokens occupying model context. Provider counters are external\n    /// input, so overflow saturates instead of panicking or wrapping.\n    pub(crate) fn context_tokens(self) -> u64 {\n        self.input\n            .saturating_add(self.output)\n            .saturating_add(self.cache_read)\n            .saturating_add(self.cache_write)\n    }\n}\n'''
if text.count(anchor) != 1:
    raise SystemExit(f"expected one TokenUsage anchor, found {text.count(anchor)}")
text = text.replace(anchor, replacement, 1)

# Place the regression at the end of the module so strict Clippy's
# items-after-test-module lint remains satisfied.
test = '''\n#[cfg(test)]\nmod token_usage_tests {\n    use super::TokenUsage;\n\n    #[test]\n    fn context_tokens_saturate_external_counters() {\n        let usage = TokenUsage {\n            input: u64::MAX,\n            output: 1,\n            cache_read: u64::MAX,\n            cache_write: u64::MAX,\n        };\n        assert_eq!(usage.context_tokens(), u64::MAX);\n    }\n}\n'''
text = text.rstrip() + "\n" + test
path.write_text(text)

path = Path("crates/ion-core/src/runtime/effects.rs")
text = path.read_text()
old = '''                self.operation_lane_live_mut(operation_id)\n                    .expect("resident operation has an owning lane")\n                    .last_context_tokens =\n                    Some(usage.input + usage.output + usage.cache_read + usage.cache_write);\n'''
new = '''                self.operation_lane_live_mut(operation_id)\n                    .expect("resident operation has an owning lane")\n                    .last_context_tokens = Some(usage.context_tokens());\n'''
if text.count(old) != 1:
    raise SystemExit(f"expected one live usage sum anchor, found {text.count(old)}")
text = text.replace(old, new, 1)
path.write_text(text)

path = Path("crates/ion-core/src/runtime/mod.rs")
text = path.read_text()
old = '''        let last_context_tokens = latest_usage.map(|usage| {\n            usage\n                .input\n                .saturating_add(usage.output)\n                .saturating_add(usage.cache_read)\n                .saturating_add(usage.cache_write)\n        });\n'''
new = '''        let last_context_tokens = latest_usage.map(TokenUsage::context_tokens);\n'''
if text.count(old) != 1:
    raise SystemExit(f"expected one recovery usage sum anchor, found {text.count(old)}")
text = text.replace(old, new, 1)
path.write_text(text)
