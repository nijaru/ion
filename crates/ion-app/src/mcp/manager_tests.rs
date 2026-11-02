//! Cleanup failures survive a lost caller; ordinary operation errors do not
//! become new environment shutdown failures.
use super::*;

#[tokio::test]
async fn shutdown_preserves_unpublished_cleanup_errors_separately_from_operation_errors() {
    for cleanup_fails in [false, true] {
        for with_reconnect in [false, true] {
            let manager = Arc::new(McpTools {
                definitions: [(
                    "peer".into(),
                    McpServer::Stdio(crate::McpStdioServer {
                        enabled: true,
                        description: None,
                        cwd: None,
                        env: Default::default(),
                        command: "not-started".into(),
                        args: Vec::new(),
                        exposure: McpExposure::Direct,
                        tool_exposure: Default::default(),
                    }),
                )]
                .into(),
                cwd: Default::default(),
                state: Default::default(),
                catalog: Arc::new(SyncRwLock::new(Catalog::new())),
                refresh_gate: Mutex::new(()),
                closing: CancellationToken::new(),
            });
            let error = combine_cleanup::<()>(
                Err(anyhow!("replacement failed")),
                if cleanup_fails {
                    Err(anyhow!("temporary connection close failed"))
                } else {
                    Ok(())
                },
            )
            .unwrap_err();
            let failure = ReconnectFailure::from(error);
            assert_eq!(failure.cleanup_failed, cleanup_fails);
            manager
                .state
                .write()
                .unwrap()
                .retain_failure("peer", &failure);
            if with_reconnect {
                let completion = Arc::new(SyncRwLock::new(Some(Err(failure))));
                let task = tokio::spawn(async {});
                let work = Arc::new(ReconnectWork {
                    stop: CancellationToken::new(),
                    completion,
                    join: Mutex::new(ReconnectJoin { task, result: None }),
                });
                work.joined().await.unwrap().unwrap_err();
                manager
                    .state
                    .write()
                    .unwrap()
                    .reconnects
                    .insert("peer".into(), work);
            }
            assert_eq!(
                manager.snapshot()[0].status,
                if cleanup_fails {
                    McpServerStatus::CleanupFailed
                } else {
                    McpServerStatus::Unavailable
                }
            );
            let mut refused = true;
            if cleanup_fails {
                match manager.reconnect("peer") {
                    Ok(mut retry) => {
                        refused = false;
                        retry.request_stop();
                        let _ = retry.join().await;
                    }
                    Err(error) => assert!(error.to_string().contains("cleanup"), "{error:#}"),
                }
            }
            let result = manager.shutdown().await;
            assert!(
                refused,
                "an unresolved cleanup failure must not be overwritten by retry"
            );
            assert_eq!(result.is_err(), cleanup_fails);
            if let Err(error) = result {
                assert!(
                    error
                        .to_string()
                        .contains("temporary connection close failed")
                );
            }
        }
    }
}
