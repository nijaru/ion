//! Owned protocol services, HTTP workers and local root processes.
use std::{future::Future, path::Path, process::Stdio, sync::Arc, time::Duration};

use super::{
    ChangeHandler, Client, McpServer,
    framing::{BoundedLineReader, MAX_MCP_MESSAGE_BYTES},
    http::OwnedHttpClient,
    send::Tracked,
    stderr,
};
use anyhow::{Context, Result, ensure};
use rmcp::{
    RoleClient,
    service::{QuitReason, RxJsonRpcMessage, ServiceExt, TxJsonRpcMessage},
    transport::{
        StreamableHttpClientTransport, Transport,
        async_rw::AsyncRwTransport,
        common::client_side_sse::SseRetryPolicy,
        streamable_http_client::{StreamableHttpClientTransportConfig, StreamableHttpError},
    },
};
use tokio::{
    process::{Child, Command},
    sync::oneshot,
};

type HttpTransport = StreamableHttpClientTransport<OwnedHttpClient>;

/// One bounded SSE retry: stream failures surface instead of retrying forever.
#[derive(Debug)]
struct OneRetry;

impl SseRetryPolicy for OneRetry {
    fn retry(&self, times: usize) -> Option<Duration> {
        (times == 0).then(|| Duration::from_millis(500))
    }
}

pub(super) struct Connection {
    pub client: Client,
    owned: OwnedTransport,
}

impl Connection {
    pub async fn start(
        name: &str,
        definition: McpServer,
        cwd: &Path,
        handler: ChangeHandler,
    ) -> Result<Self> {
        let (initialized, owned) = match definition {
            McpServer::Stdio(server) => {
                let diagnostics = stderr::Destination::create()
                    .with_context(|| format!("cannot create MCP server {name} stderr capture"))?;
                let mut command = Command::new(&server.command);
                command
                    .args(&server.args)
                    .current_dir(cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                let mut process = command
                    .spawn()
                    .with_context(|| format!("cannot start MCP server {name}"))?;
                // These handles are guaranteed by the piped configuration above.
                let stdout = process.stdout.take().expect("MCP stdout is piped");
                let stdin = process.stdin.take().expect("MCP stdin is piped");
                let stderr = diagnostics.start(process.stderr.take().expect("MCP stderr is piped"));
                let initialized = initialize(
                    name,
                    handler.serve(Tracked(AsyncRwTransport::new_client(
                        BoundedLineReader::new(stdout, MAX_MCP_MESSAGE_BYTES),
                        stdin,
                    ))),
                )
                .await;
                (initialized, OwnedTransport::Stdio { process, stderr })
            }
            McpServer::Http(server) => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(server.url);
                // A 404 can mean a lost MCP session, but a tool may already have
                // executed. Never transparently reinitialize and repeat it.
                config.reinit_on_expired_session = false;
                // The SDK's default unbounded exponential backoff can leave an
                // unanswered call retrying a broken stream indefinitely. One
                // short retry then failure keeps a lost response observable.
                config.retry_config = Arc::new(OneRetry);
                if let Some(env_name) = server.bearer_token_env {
                    let token = std::env::var(&env_name)
                        .with_context(|| format!("MCP server {name} needs {env_name}"))?;
                    ensure!(!token.is_empty(), "MCP server {name} needs {env_name}");
                    config.auth_header = Some(token);
                }
                let http_client = OwnedHttpClient::new()
                    .with_context(|| format!("MCP server {name} HTTP setup failed"))?;
                let transport =
                    StreamableHttpClientTransport::with_client(http_client.clone(), config);
                let (returned, receiver) = oneshot::channel();
                let initialized = initialize(
                    name,
                    handler.serve(Tracked(HttpLease {
                        transport: Some(transport),
                        returned: Some(returned),
                    })),
                )
                .await;
                (
                    initialized,
                    OwnedTransport::Http {
                        returned: receiver,
                        client: http_client,
                    },
                )
            }
        };
        match initialized {
            Ok(client) => Ok(Self { client, owned }),
            Err(error) => {
                // Initialization drops the pipes/lease on error or timeout.
                // Unlike rmcp's child transport, cleanup stays in this future.
                // rmcp awaits the bootstrap HTTP request/body outside its
                // worker cancellation loop. Interrupt it before joining.
                if let OwnedTransport::Http { client, .. } = &owned {
                    client.stop.cancel();
                }
                let stderr = owned.stderr_log();
                let result = combine_cleanup(Err(error), owned.shutdown().await);
                with_diagnostics(result, stderr, name)
            }
        }
    }

    pub fn stderr_log(&self) -> Option<Arc<stderr::Log>> {
        self.owned.stderr_log()
    }

    /// The reason a framing or transport failure retired this connection.
    pub fn transport_failure(&self) -> Option<String> {
        self.owned.transport_failure()
    }

    pub async fn shutdown(mut self) -> Result<()> {
        // Retirement interrupts in-flight POSTs as well as failed startup.
        // Waiting for a remote response is not a local cleanup requirement.
        if let OwnedTransport::Http { client, .. } = &self.owned {
            client.stop.cancel();
        }
        // rmcp's close_with_timeout consumes then drops the join handle at the
        // deadline. Join without that timeout; process escalation is ours.
        // A blocked SDK send can hold the stdio writer lock while service close
        // waits for it. Escalate/reap the root concurrently so closing its pipes
        // releases that send; neither cleanup future is detached.
        let stderr = self.owned.stderr_log();
        let (service, cleanup) = tokio::join!(self.client.close(), self.owned.shutdown());
        let service = match service {
            Ok(QuitReason::JoinError(error)) | Err(error) => {
                Err(error).context("MCP service cleanup failed")
            }
            Ok(_) => Ok(()),
        };
        with_diagnostics(combine_cleanup(service, cleanup), stderr, "connection")
    }
}

fn with_diagnostics<T>(result: Result<T>, log: Option<Arc<stderr::Log>>, name: &str) -> Result<T> {
    match log.and_then(|log| log.notice(name)) {
        Some(notice) => result.context(notice),
        None => result,
    }
}

async fn initialize(
    name: &str,
    work: impl Future<Output = std::result::Result<Client, rmcp::service::ClientInitializeError>>,
) -> Result<Client> {
    tokio::time::timeout(Duration::from_secs(10), work)
        .await
        .with_context(|| format!("MCP server {name} initialization timed out"))?
        .with_context(|| format!("MCP server {name} initialization failed"))
}

enum OwnedTransport {
    Stdio {
        process: Child,
        stderr: stderr::Capture,
    },
    Http {
        returned: oneshot::Receiver<HttpTransport>,
        client: OwnedHttpClient,
    },
}

impl OwnedTransport {
    fn stderr_log(&self) -> Option<Arc<stderr::Log>> {
        match self {
            Self::Stdio { stderr, .. } => Some(stderr.log.clone()),
            Self::Http { .. } => None,
        }
    }

    fn transport_failure(&self) -> Option<String> {
        match self {
            Self::Stdio { .. } => None,
            Self::Http { client, .. } => client.failure(),
        }
    }

    async fn shutdown(self) -> Result<()> {
        match self {
            Self::Stdio {
                mut process,
                mut stderr,
            } => {
                let reaped =
                    match tokio::time::timeout(Duration::from_secs(3), process.wait()).await {
                        Ok(result) => result.map(|_| ()).context("cannot reap MCP server process"),
                        Err(_) => {
                            // kill() waits as well as signalling. The retained close
                            // task owns both the root and diagnostic reader throughout.
                            process
                                .kill()
                                .await
                                .context("cannot kill and reap MCP server process")
                        }
                    };
                let captured = stderr
                    .finish()
                    .await
                    .map_err(anyhow::Error::msg)
                    .context("MCP stderr capture failed");
                combine_cleanup(reaped, captured)?;
            }
            Self::Http { returned, client } => {
                let cleanup = match returned.await {
                    Ok(mut transport) => transport
                        .close()
                        .await
                        .context("MCP HTTP worker cleanup failed"),
                    Err(error) => Err(error).context("MCP HTTP transport was not returned"),
                };
                client.join_io().await;
                cleanup?;
            }
        }
        Ok(())
    }
}

pub(super) fn combine_cleanup<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("cleanup also failed: {cleanup:#}")))
        }
    }
}

// rmcp drops transports on initialization failure and logs rather than returns
// close errors in its service loop. This concrete lease returns the HTTP worker
// to our owner on both paths; the owner explicitly closes and joins it.
struct HttpLease {
    transport: Option<HttpTransport>,
    returned: Option<oneshot::Sender<HttpTransport>>,
}

impl Drop for HttpLease {
    fn drop(&mut self) {
        if let (Some(returned), Some(transport)) = (self.returned.take(), self.transport.take()) {
            let _ = returned.send(transport);
        }
    }
}

impl Transport<RoleClient> for HttpLease {
    type Error = StreamableHttpError<reqwest::Error>;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = std::result::Result<(), Self::Error>> + Send + 'static {
        self.transport
            .as_mut()
            .expect("HTTP lease owns transport")
            .send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        self.transport
            .as_mut()
            .expect("HTTP lease owns transport")
            .receive()
            .await
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        // Worker closure is performed and checked by the owner after this
        // lease is dropped, not in rmcp's error-suppressing service loop.
        Ok(())
    }
}
