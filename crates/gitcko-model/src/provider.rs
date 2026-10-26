// SPDX-License-Identifier: AGPL-3.0-or-later

use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::{Event, ProviderError, Request};

/// The events of one call, as they arrive.
pub type EventStream = BoxStream<'static, Result<Event, ProviderError>>;

/// One binding's way to run a call: an adapter, a cassette, or the fake below.
///
/// It returns a boxed stream rather than being an `async fn`, so `dyn ModelProvider`
/// works without the `async-trait` crate. Dropping the stream, or canceling the
/// token, must stop the call and release its connection.
pub trait ModelProvider: Send + Sync {
    /// Starts the call. Errors arrive as stream items, never as a panic.
    fn stream(&self, request: Request, cancel: CancellationToken) -> EventStream;
}

/// A provider that plays back scripted responses, one per call, and counts calls.
#[cfg(feature = "fake")]
#[derive(Default)]
pub struct FakeProvider {
    script: std::sync::Mutex<std::collections::VecDeque<Vec<Result<Event, ProviderError>>>>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "fake")]
impl FakeProvider {
    /// Each item is the full response to one call, in call order.
    pub fn new(script: Vec<Vec<Result<Event, ProviderError>>>) -> FakeProvider {
        FakeProvider {
            script: std::sync::Mutex::new(script.into()),
            calls: Default::default(),
        }
    }

    /// How many calls have started.
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(feature = "fake")]
impl ModelProvider for FakeProvider {
    fn stream(&self, _request: Request, _cancel: CancellationToken) -> EventStream {
        use futures_util::StreamExt;
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // An unscripted call is a test bug; fail it loudly but as a stream item.
        let events = self.script.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![Err(ProviderError::new(crate::ErrorKind::InvalidResponse(
                "the fake provider has no response scripted for this call",
            )))]
        });
        futures_util::stream::iter(events).boxed()
    }
}
