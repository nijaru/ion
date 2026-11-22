//! A single cancellable, read-only refresh for the selected terminal binding.
use std::{sync::Arc, time::Duration};

use ion_ai::BoxFuture;
use ion_app::{Balance, ProviderBalance};
use tokio::time::Instant;

const REFRESH: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct BalanceStatus {
    source: Option<Arc<ProviderBalance>>,
    next: Option<Instant>,
    pending: Option<BoxFuture<'static, Option<Balance>>>,
    current: Option<(Instant, Balance)>,
}

impl BalanceStatus {
    pub(super) fn bind(&mut self, source: Option<&Arc<ProviderBalance>>) {
        if match (&self.source, source) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        } {
            return;
        }
        self.source = source.cloned();
        self.invalidate();
    }

    pub(super) fn credential_changed(&mut self, provider: &str) {
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.provider() == provider)
        {
            self.invalidate();
        }
    }

    fn invalidate(&mut self) {
        self.pending = None;
        self.current = None;
        self.next = self.source.as_ref().map(|_| Instant::now());
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.next
    }

    pub(super) fn start(&mut self) {
        self.next = None;
        let source = self.source.clone().expect("bound refresh source");
        self.pending = Some(Box::pin(async move { source.read().await }));
    }

    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) async fn complete(&mut self) {
        let balance = self.pending.as_mut().expect("pending refresh").await;
        self.pending = None;
        let next = Instant::now() + REFRESH;
        self.current = balance.map(|balance| (next + Duration::from_secs(5), balance));
        self.next = Some(next);
    }

    pub(super) fn current(&self) -> Option<&Balance> {
        self.current
            .as_ref()
            .filter(|(expires, _)| Instant::now() < *expires)
            .map(|(_, balance)| balance)
    }
}
