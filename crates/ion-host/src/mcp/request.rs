//! One MCP request's connection lease, local send and observed response.
use std::time::Duration;

use rmcp::{
    model::{ClientRequest, ServerResult},
    service::PeerRequestOptions,
};
use tokio_util::sync::CancellationToken;

use super::{Server, send::Completion};

pub(super) enum RequestError {
    Closed,
    Cancelled,
    Retired,
    Failed(String),
}

impl Server {
    pub(super) async fn request(
        &self,
        mut request: ClientRequest,
        stop: &CancellationToken,
    ) -> Result<(ServerResult, Option<String>), RequestError> {
        let sent = Completion::attach(&mut request);
        let client = tokio::select! {
            biased;
            () = self.retiring.cancelled() => return Err(RequestError::Closed),
            () = stop.cancelled() => return Err(RequestError::Cancelled),
            client = self.client.read() => client,
        };
        let mut handle = {
            let Some(connection) = client.as_ref() else {
                return Err(RequestError::Closed);
            };
            // no_options has no awaited work after queue admission: cancelling
            // pending admission cannot discard an already issued request.
            tokio::select! {
                biased;
                () = self.retiring.cancelled() => return Err(RequestError::Closed),
                () = stop.cancelled() => return Err(RequestError::Cancelled),
                handle = connection.client.send_cancellable_request(request, PeerRequestOptions::no_options()) => {
                    handle.map_err(|error| RequestError::Failed(error.to_string()))?
                }
            }
        };
        let response = tokio::select! {
            biased;
            response = &mut handle.rx => Some(response),
            () = self.retiring.cancelled() => None,
            () = stop.cancelled() => {
                // Release the read lease before retirement takes the connection.
                drop(client);
                let stopping = async {
                    tokio::join!(sent, handle.cancel(Some("Ion operation cancelled".into())))
                };
                tokio::pin!(stopping);
                // A local flush deadline, not a remote execution timeout. An
                // incomplete send must join retirement before the caller returns.
                match tokio::time::timeout(Duration::from_secs(3), &mut stopping).await {
                    Ok((_, Ok(()))) => {}
                    Ok((_, Err(error))) => {
                        self.shutdown().await.map_err(|error| RequestError::Failed(error.to_string()))?;
                        return Err(RequestError::Failed(error.to_string()));
                    }
                    Err(_) => {
                        let cleanup = self.shutdown().await;
                        let _ = stopping.await;
                        cleanup.map_err(|error| RequestError::Failed(error.to_string()))?;
                    }
                }
                return Err(RequestError::Cancelled);
            }
        };
        // Retirement may take the connection as soon as this lease is released.
        // Retain its recorded cause before joining any concurrent cleanup.
        let transport_failure = client
            .as_ref()
            .and_then(|connection| connection.transport_failure());
        drop(client);
        let Some(response) = response else {
            let cleanup = self.shutdown().await;
            let _ = sent.await;
            cleanup.map_err(|error| RequestError::Failed(error.to_string()))?;
            return Err(transport_failure.map_or(RequestError::Retired, RequestError::Failed));
        };
        // Observing the response does not prove the SDK's send task has joined.
        // Preserve that response even if stop/retirement interrupts a stuck send.
        let mut sent = sent;
        let closing = async {
            tokio::select! {
                () = stop.cancelled() => {}
                () = self.retiring.cancelled() => {}
            }
        };
        let cleanup_error = tokio::select! {
            biased;
            _ = &mut sent => None,
            () = closing => {
                let cleanup = self.shutdown().await;
                let _ = sent.await;
                cleanup.err().map(|error| format!("{error:#}"))
            }
        };
        let response = response
            .map_err(|_| "transport closed".to_owned())
            .and_then(|response| response.map_err(|error| error.to_string()));
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                // rmcp may reduce a framing rejection to transport closure.
                // Prefer the recorded transport cause for every request kind.
                let reason = transport_failure.unwrap_or(error);
                return Err(RequestError::Failed(reason));
            }
        };
        Ok((response, cleanup_error))
    }
}
