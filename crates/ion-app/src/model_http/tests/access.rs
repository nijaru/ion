use super::*;
use crate::{CredentialResolutionError, CredentialStore};

fn authentication<T>(result: Result<T, ProviderError>) {
    let error = result.err().expect("access must be rejected");
    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert!(!error.message.contains("secret"));
}

async fn no_connection(listener: &TcpListener) {
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        "access checks must not send model transport"
    );
}

#[tokio::test]
async fn required_credential_is_checked_at_preflight_and_again_after_logout() {
    let root = std::env::temp_dir().join(format!("ion-http-access-{}", uuid::Uuid::now_v7()));
    let store = CredentialStore::new(&root);
    let provider = format!("http-access-{}", uuid::Uuid::now_v7());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let service = HttpModelService::new(
        &endpoint,
        HttpWire::ChatCompletions,
        store.resolver(&provider, None).unwrap(),
    )
    .unwrap()
    .with_required_credential(true);

    authentication(service.validate_access().await);
    authentication(service.stream(request()).await);
    no_connection(&listener).await;

    store.save_api_key(&provider, "secret-current-key").unwrap();
    service.validate_access().await.unwrap();
    no_connection(&listener).await;
    store.remove(&provider).unwrap();
    authentication(service.stream(request()).await);
    no_connection(&listener).await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_credentials_are_sanitized_before_transport_even_on_optional_routes() {
    for wire in [HttpWire::ChatCompletions, HttpWire::AnthropicMessages] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        for key in [
            "",
            "secret\r\ninjected: header",
            "secret\tinvalid",
            "secret\0invalid",
        ] {
            let service =
                HttpModelService::new(&endpoint, wire, Arc::new(move || Some(key.into()))).unwrap();
            authentication(service.validate_access().await);
            authentication(service.stream(request()).await);
        }
        struct Unavailable;
        impl CredentialResolver for Unavailable {
            fn resolve<'a>(
                &'a self,
                _: CancellationToken,
            ) -> BoxFuture<'a, Result<Option<String>, CredentialResolutionError>> {
                Box::pin(async { Err(CredentialResolutionError::Unavailable) })
            }
        }
        let service = HttpModelService::new(&endpoint, wire, Arc::new(Unavailable)).unwrap();
        authentication(service.validate_access().await);
        authentication(service.stream(request()).await);
        no_connection(&listener).await;
    }
}

#[tokio::test]
async fn required_and_optional_routes_dispatch_with_the_selected_wire_header() {
    for (wire, key) in [
        (HttpWire::ChatCompletions, Some("secret-current-key")),
        (HttpWire::AnthropicMessages, Some("secret-current-key")),
        (HttpWire::ChatCompletions, None),
        (HttpWire::AnthropicMessages, None),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
                headers.push_str(&line.to_ascii_lowercase());
            }
            reader.read_exact(&mut vec![0; length]).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            headers
        });
        let service =
            HttpModelService::new(&endpoint, wire, Arc::new(move || key.map(str::to_owned)))
                .unwrap()
                .with_required_credential(key.is_some());
        service.validate_access().await.unwrap();
        drop(service.stream(request()).await.unwrap());
        let headers = server.await.unwrap();
        if key.is_none() {
            assert!(!headers.contains("authorization:") && !headers.contains("x-api-key:"));
        } else if wire == HttpWire::AnthropicMessages {
            assert!(headers.contains("x-api-key: secret-current-key\r\n"));
            assert!(!headers.contains("authorization:"));
        } else {
            assert!(headers.contains("authorization: bearer secret-current-key\r\n"));
            assert!(!headers.contains("x-api-key:"));
        }
    }
}
