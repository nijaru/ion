//! Concrete image-preparation stop-and-join custody, independent of Core's slot.
use anyhow::{Result, anyhow};
use ion_ai::Message;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct Retained {
    task: Option<JoinHandle<Result<Message>>>,
}
pub(crate) struct Preparation {
    stop: CancellationToken,
    retained: Arc<Mutex<Retained>>,
}
impl Preparation {
    pub(crate) fn start<F, W>(work: F) -> Self
    where
        F: FnOnce(CancellationToken) -> W + Send + 'static,
        W: Future<Output = Result<Message>> + Send + 'static,
    {
        let stop = CancellationToken::new();
        let retained = Arc::new(Mutex::new(Retained { task: None }));
        let mut state = retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let custody = retained.clone();
        let token = stop.clone();
        // The worker retains its custodian through completion even if the control
        // is dropped. Unlike an aborted/dropped decoder future, this always joins
        // the blocking decoder. Explicit shutdown also retrieves its result.
        state.task = Some(tokio::spawn(async move {
            let _custody = custody;
            work(token).await
        }));
        drop(state);
        Self { stop, retained }
    }
    pub(crate) fn request_stop(&self) {
        self.stop.cancel();
    }
    pub(crate) async fn join(&mut self) -> Result<Message> {
        poll_fn(|cx| {
            let mut retained = self
                .retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let task = retained
                .task
                .as_mut()
                .expect("preparation completion consumed once");
            match Pin::new(task).poll(cx) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(result) => {
                    retained.task = None;
                    std::task::Poll::Ready(
                        result.unwrap_or_else(|error| {
                            Err(anyhow!("preparation task failed: {error}"))
                        }),
                    )
                }
            }
        })
        .await
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
