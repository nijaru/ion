from pathlib import Path

path = Path("crates/ion/src/openrouter.rs")
text = path.read_text()

old = """            // Accumulated tool calls by stream index: (id, name, args).\n            let mut tool_calls: Vec<(Option<String>, String, String)> = Vec::new();\n\n            'stream: loop {\n"""
new = """            // Accumulated tool calls by stream index: (id, name, args).\n            let mut tool_calls: Vec<(Option<String>, String, String)> = Vec::new();\n            let mut saw_done = false;\n\n            'stream: loop {\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one stream state anchor, found {text.count(old)}")
text = text.replace(old, new, 1)

old = """                    if payload == \"[DONE]\" {\n                        buffer.clear();\n                        break 'stream;\n                    }\n"""
new = """                    if payload == \"[DONE]\" {\n                        saw_done = true;\n                        buffer.clear();\n                        break 'stream;\n                    }\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one DONE anchor, found {text.count(old)}")
text = text.replace(old, new, 1)

old = """                // No early break on finish_reason: the usage chunk (and\n                // any trailing provider metadata) arrives after it; the\n                // stream ends at [DONE] or EOF.\n            }\n\n            // The step is complete: emit whole tool calls only (§15.2).\n"""
new = """                // No early break on finish_reason: the usage chunk (and\n                // any trailing provider metadata) arrives after it. Only\n                // OpenRouter's [DONE] marker proves semantic completion.\n            }\n\n            if !saw_done {\n                let _ = out\n                    .send(EngineSignal::Failed {\n                        operation_id,\n                        step,\n                        message: \"provider stream ended before [DONE]\".to_owned(),\n                    })\n                    .await;\n                return;\n            }\n\n            // The step is complete: emit whole tool calls only (§15.2).\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one completion anchor, found {text.count(old)}")
text = text.replace(old, new, 1)

anchor = """    #[tokio::test]\n    async fn done_marker_terminates_stream_without_waiting_for_eof() {\n"""
test = """    #[tokio::test]\n    async fn eof_before_done_fails_instead_of_completing_partial_output() {\n        let base_url = spawn_sse_server(\n            \"data: {\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"partial\\\"}}]}\\n\\n\\\n             data: {\\\"choices\\\":[{\\\"delta\\\":{},\\\"finish_reason\\\":\\\"stop\\\"}]}\\n\\n\",\n            None,\n        );\n        let provider = OpenRouterProvider::new(\"test/model\", \"key\").with_base_url(base_url);\n        let signals = collect(provider).await;\n\n        assert!(signals.iter().any(|signal| matches!(\n            signal,\n            EngineSignal::TextDelta { text, .. } if text == \"partial\"\n        )));\n        assert!(\n            !signals\n                .iter()\n                .any(|signal| matches!(signal, EngineSignal::Completed { .. })),\n            \"transport EOF must not promote partial provider output to completion: {signals:?}\",\n        );\n        assert!(matches!(\n            signals.last(),\n            Some(EngineSignal::Failed { message, .. })\n                if message == \"provider stream ended before [DONE]\"\n        ));\n    }\n\n"""
if text.count(anchor) != 1:
    raise SystemExit(f"expected one DONE test anchor, found {text.count(anchor)}")
text = text.replace(anchor, test + anchor, 1)

path.write_text(text)
