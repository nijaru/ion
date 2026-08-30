from pathlib import Path

path = Path("crates/ion/src/openrouter.rs")
text = path.read_text()

old = """            loop {\n                let chunk = tokio::select! {\n"""
new = """            'stream: loop {\n                let chunk = tokio::select! {\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one stream loop anchor, found {text.count(old)}")
text = text.replace(old, new, 1)

old = """                    if payload == \"[DONE]\" {\n                        buffer.clear();\n                        break;\n                    }\n"""
new = """                    if payload == \"[DONE]\" {\n                        buffer.clear();\n                        break 'stream;\n                    }\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one DONE anchor, found {text.count(old)}")
text = text.replace(old, new, 1)

anchor = """    async fn collect(provider: OpenRouterProvider) -> Vec<EngineSignal> {\n"""
helper = """    /// SSE server that deliberately keeps the chunked response open after\n    /// sending OpenRouter's semantic `[DONE]` marker. A correct client must\n    /// finish from that marker rather than waiting for transport EOF.\n    fn spawn_open_sse_server(body: &'static str) -> String {\n        let listener = TcpListener::bind(\"127.0.0.1:0\").expect(\"bind\");\n        let port = listener.local_addr().expect(\"addr\").port();\n        std::thread::spawn(move || {\n            let (mut socket, _) = listener.accept().expect(\"accept\");\n            let mut buf = vec![0u8; 16384];\n            let _ = socket.read(&mut buf);\n            let response = format!(\n                \"HTTP/1.1 200 OK\\r\\nContent-Type: text/event-stream\\r\\nTransfer-Encoding: chunked\\r\\nConnection: keep-alive\\r\\n\\r\\n{:X}\\r\\n{}\\r\\n\",\n                body.len(),\n                body,\n            );\n            socket.write_all(response.as_bytes()).expect(\"write SSE response\");\n            socket.flush().expect(\"flush SSE response\");\n            std::thread::sleep(Duration::from_secs(3));\n        });\n        format!(\"http://127.0.0.1:{port}/v1\")\n    }\n\n"""
if text.count(anchor) != 1:
    raise SystemExit(f"expected one collect anchor, found {text.count(anchor)}")
text = text.replace(anchor, helper + anchor, 1)

anchor = """    #[tokio::test]\n    async fn tool_call_fragments_become_one_completed_call() {\n"""
test = """    #[tokio::test]\n    async fn done_marker_terminates_stream_without_waiting_for_eof() {\n        let base_url = spawn_open_sse_server(\n            \"data: {\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"done\\\"}}]}\\n\\n\\\n             data: [DONE]\\n\\n\",\n        );\n        let provider = OpenRouterProvider::new(\"test/model\", \"key\").with_base_url(base_url);\n        let cancel = CancellationToken::new();\n        let (tx, mut rx) = mpsc::channel(64);\n        let request = ProviderRequest {\n            operation_id: ion_core::OperationId::generate(),\n            step: 1,\n            model: ion_core::ModelConfig {\n                model_ref: \"test/model\".to_owned(),\n                context_window: None,\n                capabilities: ion_core::ModelCapabilities {\n                    reasoning: true,\n                    ..ion_core::ModelCapabilities::default()\n                },\n            },\n            plan: ContextPlan {\n                system: \"sys\".to_owned(),\n                messages: vec![ContextMessage::User {\n                    content: \"hello\".to_owned(),\n                }],\n            },\n            tools: Vec::new(),\n        };\n\n        tokio::time::timeout(Duration::from_secs(1), provider.run(request, cancel, tx))\n            .await\n            .expect(\"[DONE] must settle the provider before transport EOF\");\n\n        let mut signals = Vec::new();\n        while let Some(signal) = rx.recv().await {\n            signals.push(signal);\n        }\n        assert!(signals.iter().any(|signal| matches!(\n            signal,\n            EngineSignal::TextDelta { text, .. } if text == \"done\"\n        )));\n        assert!(matches!(\n            signals.last(),\n            Some(EngineSignal::Completed { .. })\n        ));\n    }\n\n"""
if text.count(anchor) != 1:
    raise SystemExit(f"expected one tool-call test anchor, found {text.count(anchor)}")
text = text.replace(anchor, test + anchor, 1)

path.write_text(text)
