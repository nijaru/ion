use std::collections::VecDeque;
use std::sync::Mutex;

use futures_util::stream;

use crate::{BoxFuture, ModelRequest, ModelService, ModelStream, ModelStreamEvent, ProviderError};

#[derive(Debug, Clone)]
pub enum Script {
    Stream(Vec<ModelStreamEvent>),
    OpenError(ProviderError),
}

pub struct ScriptedModelService {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModelService {
    pub fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("request mutex").clone()
    }
}

impl ModelService for ScriptedModelService {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            self.requests.lock().expect("request mutex").push(request);
            let script = self
                .scripts
                .lock()
                .expect("script mutex")
                .pop_front()
                .expect("scripted model service exhausted");

            match script {
                Script::OpenError(error) => Err(error),
                Script::Stream(events) => {
                    let stream: ModelStream =
                        Box::pin(stream::iter(events.into_iter().map(Ok)));
                    Ok(stream)
                }
            }
        })
    }
}
