from pathlib import Path

path = Path("crates/ion/src/openrouter.rs")
text = path.read_text()

old = '''                let url = format!("{}/models/{}", self.base_url, self.model);\n'''
new = '''                let url = format!("{}/model/{}", self.base_url, self.model);\n'''
if text.count(old) != 1:
    raise SystemExit(f"expected one metadata URL anchor, found {text.count(old)}")
text = text.replace(old, new, 1)

anchor = '''    async fn collect(provider: OpenRouterProvider) -> Vec<EngineSignal> {\n'''
helper = '''    fn spawn_json_server(\n        body: &'static str,\n        captured_request: std::sync::mpsc::Sender<String>,\n    ) -> String {\n        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");\n        let port = listener.local_addr().expect("addr").port();\n        std::thread::spawn(move || {\n            let (mut socket, _) = listener.accept().expect("accept");\n            let mut buf = vec![0u8; 16384];\n            let n = socket.read(&mut buf).unwrap_or(0);\n            let request = String::from_utf8_lossy(&buf[..n]).into_owned();\n            let _ = captured_request.send(request);\n            let response = format!(\n                "HTTP/1.1 200 OK\\r\\nContent-Type: application/json\\r\\nContent-Length: {}\\r\\nConnection: close\\r\\n\\r\\n{body}",\n                body.len()\n            );\n            socket.write_all(response.as_bytes()).expect("write response");\n        });\n        format!("http://127.0.0.1:{port}/v1")\n    }\n\n'''
if text.count(anchor) != 1:
    raise SystemExit(f"expected one collect anchor, found {text.count(anchor)}")
text = text.replace(anchor, helper + anchor, 1)

old = '''    #[tokio::test]\n    async fn context_window_fetched_from_models_endpoint() {\n        let base_url = spawn_sse_server(\n            "{\\\"data\\\":{\\\"id\\\":\\\"test/model\\\",\\\"context_length\\\":1000000}}",\n            None,\n        );\n        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);\n        assert_eq!(provider.context_window().await, Some(1_000_000));\n    }\n'''
new = '''    #[tokio::test]\n    async fn context_window_uses_single_model_metadata_route() {\n        let (tx, rx) = std::sync::mpsc::channel();\n        let base_url = spawn_json_server(\n            "{\\\"data\\\":{\\\"id\\\":\\\"test/model\\\",\\\"context_length\\\":1000000}}",\n            tx,\n        );\n        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);\n        assert_eq!(provider.context_window().await, Some(1_000_000));\n\n        let request = rx\n            .recv_timeout(Duration::from_secs(5))\n            .expect("metadata request");\n        assert!(\n            request.starts_with("GET /v1/model/test/model HTTP/1.1\\r\\n"),\n            "unexpected metadata request: {request:?}",\n        );\n    }\n'''
if text.count(old) != 1:
    raise SystemExit(f"expected one context-window test anchor, found {text.count(old)}")
text = text.replace(old, new, 1)

path.write_text(text)
