from pathlib import Path

# Fixed provider adapters own the configured model identity. The core Provider
# default is only appropriate for generic/test providers with no explicit model.
for relative, marker in [
    ("crates/ion/src/openrouter.rs", "impl Provider for OpenRouterProvider {\n"),
    ("crates/ion/src/openai_codex.rs", "impl Provider for OpenAICodexProvider {\n"),
]:
    path = Path(relative)
    text = path.read_text()
    if text.count(marker) != 1:
        raise SystemExit(f"expected one Provider impl marker in {relative}, found {text.count(marker)}")
    replacement = marker + '''    fn initial_model_ref(&self) -> String {\n        self.model.clone()\n    }\n\n'''
    text = text.replace(marker, replacement, 1)
    path.write_text(text)

path = Path("crates/ion/src/lib.rs")
text = path.read_text()
marker = "impl Provider for CliProvider {\n"
if text.count(marker) != 1:
    raise SystemExit(f"expected one CliProvider impl marker, found {text.count(marker)}")
replacement = marker + '''    fn initial_model_ref(&self) -> String {\n        match self {\n            CliProvider::OpenAICodex(provider) => provider.initial_model_ref(),\n            CliProvider::OpenRouter(provider) | CliProvider::Desktop(provider) => {\n                provider.initial_model_ref()\n            }\n            // Preserve the pre-existing identity for host/test placeholders that\n            // do not own a configured external model.\n            CliProvider::Scripted(_) | CliProvider::Unavailable(_) => {\n                std::any::type_name::<Self>().to_owned()\n            }\n        }\n    }\n\n'''
text = text.replace(marker, replacement, 1)

anchor = '''/// Build the scripted-provider factory used when no real model is\n'''
test_module = '''#[cfg(test)]\nmod provider_identity_tests {\n    use super::*;\n\n    #[tokio::test]\n    async fn raw_cli_provider_seeds_runtime_with_configured_model_identity() {\n        let store = ion_core::SessionStore::open_in_memory().expect("store");\n        let runtime = ion_core::Runtime::start_with_store(\n            CliProvider::OpenRouter(OpenRouterProvider::new("test/model", "key")),\n            ion_core::ToolRegistry::default(),\n            store,\n        );\n        let session = runtime.session();\n        let snapshot = session.snapshot().await.expect("snapshot");\n        assert_eq!(snapshot.model_ref, "test/model");\n        assert!(\n            CliProvider::OpenAICodex(OpenAICodexProvider::new(\n                "gpt-test",\n                "token",\n                "account",\n            ))\n            .supports_model("gpt-test"),\n            "fixed Codex adapters must expose their configured model identity",\n        );\n        session.close().await.expect("close");\n        runtime.join().await.expect("join");\n    }\n}\n\n'''
if text.count(anchor) != 1:
    raise SystemExit(f"expected scripted factory anchor, found {text.count(anchor)}")
text = text.replace(anchor, test_module + anchor, 1)
path.write_text(text)
