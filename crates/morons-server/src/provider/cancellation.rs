use std::future;

use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct ProviderCancellationHandle {
    sender: watch::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct ProviderCancellation {
    receiver: watch::Receiver<bool>,
}

#[must_use]
pub fn provider_cancellation() -> (ProviderCancellationHandle, ProviderCancellation) {
    let (sender, receiver) = watch::channel(false);
    (
        ProviderCancellationHandle { sender },
        ProviderCancellation { receiver },
    )
}

impl ProviderCancellationHandle {
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

impl ProviderCancellation {
    pub(crate) fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub(super) async fn cancelled(&mut self) {
        loop {
            if *self.receiver.borrow_and_update() {
                return;
            }
            if self.receiver.changed().await.is_err() {
                future::pending::<()>().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::provider_cancellation;

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_is_idempotent_and_observable() {
        let (handle, mut cancellation) = provider_cancellation();
        assert!(!cancellation.is_cancelled());
        handle.cancel();
        handle.cancel();
        cancellation.cancelled().await;
        assert!(cancellation.is_cancelled());
    }
}
