//! Owned protocol services, HTTP workers and local root processes.
use std::{future::Future, path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use rmcp::{
    RoleClient,
    service::{QuitReason, RxJsonRpcMessage, ServiceExt, TxJsonRpcMessage},
    transport::{
        StreamableHttpClientTransport, Transport,
        streamable_http_client::{StreamableHttpClientTransportConfig, StreamableHttpError},
    },
};
use tokio::{
    process::{Child, Command},
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;

use super::{ChangeHandler, Client, McpServer, http::StartupHttpClient};

type HttpTransport = StreamableHttpClientTransport<StartupHttpClient>;

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
                let mut command = Command::new(&server.command);
                command
                    .args(&server.args)
                    .current_dir(cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .kill_on_drop(true);
                let mut process = command
                    .spawn()
                    .with_context(|| format!("cannot start MCP server {name}"))?;
                // These handles are guaranteed by the piped configuration above.
                let stdout = process.stdout.take().expect("MCP stdout is piped");
                let stdin = process.stdin.take().expect("MCP stdin is piped");
                let initialized = initialize(name, handler.serve((stdout, stdin))).await;
                (initialized, OwnedTransport::Stdio(process))
            }
            McpServer::Http(server) => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(server.url);
                // A 404 can mean a lost MCP session, but a tool may already have
                // executed. Never transparently reinitialize and repeat it.
                config.reinit_on_expired_session = false;
                if let Some(env_name) = server.bearer_token_env {
                    let token = std::env::var(&env_name)
                        .with_context(|| format!("MCP server {name} needs {env_name}"))?;
                    ensure!(!token.is_empty(), "MCP server {name} needs {env_name}");
                    config.auth_header = Some(token);
                }
                let http_client = StartupHttpClient::new()
                    .with_context(|| format!("MCP server {name} HTTP setup failed"))?;
                let stop = http_client.stop.clone();
                let transport = StreamableHttpClientTransport::with_client(http_client, config);
                let (returned, receiver) = oneshot::channel();
                let initialized = initialize(
                    name,
                    handler.serve(HttpLease {
                        transport: Some(transport),
                        returned: Some(returned),
                    }),
                )
                .await;
                (
                    initialized,
                    OwnedTransport::Http {
                        returned: receiver,
                        stop,
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
                if let OwnedTransport::Http { stop, .. } = &owned {
                    stop.cancel();
                }
                combine_cleanup(Err(error), owned.shutdown().await)
            }
        }
    }

    pub async fn shutdown(mut self) -> Result<()> {
        // rmcp's close_with_timeout consumes then drops the join handle at the
        // deadline. Join without that timeout; process escalation is ours.
        let service = match self.client.close().await {
            Ok(QuitReason::JoinError(error)) | Err(error) => {
                Err(error).context("MCP service cleanup failed")
            }
            Ok(_) => Ok(()),
        };
        combine_cleanup(service, self.owned.shutdown().await)
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
    Stdio(Child),
    Http {
        returned: oneshot::Receiver<HttpTransport>,
        stop: CancellationToken,
    },
}

impl OwnedTransport {
    async fn shutdown(self) -> Result<()> {
        match self {
            Self::Stdio(mut child) => {
                match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                    Ok(result) => {
                        result.context("cannot reap MCP server process")?;
                    }
                    Err(_) => {
                        // kill() waits as well as signalling. Keep ownership
                        // through this await; kill_on_drop alone does not reap.
                        child
                            .kill()
                            .await
                            .context("cannot kill and reap MCP server process")?;
                    }
                }
            }
            Self::Http { returned, .. } => {
                let mut transport = returned
                    .await
                    .context("MCP HTTP transport was not returned")?;
                transport
                    .close()
                    .await
                    .context("MCP HTTP worker cleanup failed")?;
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
