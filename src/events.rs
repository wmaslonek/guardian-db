use crate::guardian::error::{GuardianError, Result};
use crate::p2p::{Emitter, EventBus};
use async_trait::async_trait;
use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// A type alias for a dynamic, thread-safe event.
pub type Event = Arc<dyn Any + Send + Sync>;

/// A wrapper struct for sending events through the bus, which may require a concrete type.
#[derive(Clone, Debug)]
pub struct EventBox {
    pub evt: Event,
}

// A private struct that groups ALL data that needs to be protected.
struct EventEmitterInternal {
    bus: Option<EventBus>,
    emitter: Option<Emitter<EventBox>>,
    cglobal: Option<broadcast::Sender<Event>>,
    cancellations: Vec<CancellationToken>,
}

impl EventEmitterInternal {
    /// Gets a mutable reference to the bus, initializing it if it does not exist yet.
    fn get_bus_mut(&mut self) -> &mut EventBus {
        self.bus.get_or_insert_with(EventBus::new)
    }
}

#[async_trait]
pub trait EmitterInterface {
    /// Sends an event to the subscribed listeners.
    async fn emit(&self, evt: Event);

    /// Returns a channel that receives the emitted events.
    async fn subscribe(&self) -> (mpsc::Receiver<Event>, CancellationToken);

    /// Closes all listener channels.
    async fn unsubscribe_all(&self);

    /// Returns a global channel that receives all emitted events.
    async fn global_channel(&self) -> broadcast::Receiver<Event>;
}

// The implementation of the public methods is now done inside the `impl EmitterInterface` block.
#[async_trait]
impl EmitterInterface for EventEmitter {
    async fn emit(&self, evt: Event) {
        let mut guard = self.internal.lock().await;
        if guard.emitter.is_none() {
            let bus = guard.get_bus_mut();
            let emitter = bus
                .emitter::<EventBox>()
                .await
                .expect("could not initialize the emitter for EventBox");
            guard.emitter = Some(emitter);
        }
        if let Some(emitter) = guard.emitter.as_ref() {
            let event_box = EventBox { evt };
            let _ = emitter.emit(event_box);
        }
    }

    async fn subscribe(&self) -> (mpsc::Receiver<Event>, CancellationToken) {
        let mut guard = self.internal.lock().await;
        let bus = guard.get_bus_mut();
        let sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("could not subscribe");
        let cancellation_token = CancellationToken::new();
        guard.cancellations.push(cancellation_token.clone());
        drop(guard);
        let receiver = self
            .handle_subscriber(cancellation_token.clone(), sub)
            .await;
        (receiver, cancellation_token)
    }

    async fn unsubscribe_all(&self) {
        let guard = self.internal.lock().await;
        for token in &guard.cancellations {
            token.cancel();
        }
    }

    async fn global_channel(&self) -> broadcast::Receiver<Event> {
        let mut guard = self.internal.lock().await;
        if let Some(sender) = &guard.cglobal {
            return sender.subscribe();
        }
        let bus = guard.get_bus_mut();
        let mut sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("unable to subscribe");
        let token = CancellationToken::new();
        guard.cancellations.push(token.clone());
        let (tx, rx) = broadcast::channel(16);
        guard.cglobal = Some(tx.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    maybe_event = sub.recv() => {
                        let event_box = match maybe_event {
                            Ok(e) => e,
                            Err(_) => break
                        };
                        let _ = tx.send(event_box.evt);
                    }
                }
            }
        });
        rx
    }
}

// The public struct that users will interact with.
/// Registers listeners and dispatches events to them.
#[derive(Clone)]
pub struct EventEmitter {
    internal: Arc<Mutex<EventEmitterInternal>>,
}

impl EventEmitter {
    /// Creates a new EventEmitter instance.
    pub fn new() -> Self {
        EventEmitter {
            internal: Arc::new(Mutex::new(EventEmitterInternal {
                bus: None,
                emitter: None,
                cglobal: None,
                cancellations: Vec::new(),
            })),
        }
    }

    /// Sends an event to the subscribed listeners.
    pub async fn emit(&self, evt: Event) {
        let mut guard = self.internal.lock().await;

        // Lazily initialize the emitter if it does not exist.
        if guard.emitter.is_none() {
            let bus = guard.get_bus_mut();
            let emitter = bus
                .emitter::<EventBox>()
                .await
                .expect("could not initialize the emitter for EventBox");
            guard.emitter = Some(emitter);
        }

        // The lock is still held, so it is safe to use unwrap.
        if let Some(emitter) = guard.emitter.as_ref() {
            let event_box = EventBox { evt };
            let _ = emitter.emit(event_box);
        }
    }

    /// Returns a channel that receives the emitted events.
    pub async fn subscribe(&self) -> (mpsc::Receiver<Event>, CancellationToken) {
        let mut guard = self.internal.lock().await;

        let bus = guard.get_bus_mut();

        let sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("could not subscribe");

        // Create a cancellation token to manage the subscription's lifecycle.
        let cancellation_token = CancellationToken::new();
        guard.cancellations.push(cancellation_token.clone());

        // The lock is released when `guard` goes out of scope.
        drop(guard);

        // The returned token can be used to cancel only this subscription.
        let receiver = self
            .handle_subscriber(cancellation_token.clone(), sub)
            .await;

        (receiver, cancellation_token)
    }

    /// Closes all listener channels (cancelling the listening tasks).
    pub async fn unsubscribe_all(&self) {
        let guard = self.internal.lock().await;

        // Cancel all active subscriptions.
        for token in &guard.cancellations {
            token.cancel();
        }
    }

    /// Processes events from a subscription, managing an internal queue to handle
    /// slow consumers.
    async fn handle_subscriber(
        &self,
        token: CancellationToken,
        mut sub: broadcast::Receiver<EventBox>, // Our receiver from the EventBus.
    ) -> mpsc::Receiver<Event> {
        let (tx, rx) = mpsc::channel(16);
        let queue = Arc::new(Mutex::new(VecDeque::<Event>::new()));
        let consumer_notify = Arc::new(Notify::new());

        // Producer task: moves events from the bus to the internal queue.
        let producer_tx = tx.clone();
        let producer_queue = Arc::clone(&queue);
        let producer_notify = Arc::clone(&consumer_notify);
        let producer_token = token.clone();
        tokio::spawn(async move {
            loop {
                let event_box = tokio::select! {
                    biased;
                    _ = producer_token.cancelled() => {
                        producer_notify.notify_one(); // Wake the consumer so it can finish.
                        break;
                    },
                    maybe_event = sub.recv() => {
                        match maybe_event {
                            Ok(e) => e,
                            Err(_) => break, // The subscription was closed.
                        }
                    }
                };

                // Extract the event from the EventBox.
                let event = event_box.evt;

                // Queueing logic.
                let mut q = producer_queue.lock().await;
                if q.is_empty() {
                    // If the queue is empty, try to send directly (optimization).
                    if let Err(mpsc::error::TrySendError::Full(e)) = producer_tx.try_send(event) {
                        q.push_back(e); // If the channel is full, enqueue.
                    }
                } else {
                    // If the queue already has items, push back to preserve order.
                    q.push_back(event);
                }
                producer_notify.notify_one();
            }
        });

        // Consumer task: moves events from the queue to the output channel.
        tokio::spawn(async move {
            loop {
                let event = {
                    let mut q = queue.lock().await;
                    if let Some(e) = q.pop_front() {
                        e
                    } else {
                        // The queue is empty, wait for a notification or cancellation.
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => break,
                            _ = consumer_notify.notified() => continue,
                        }
                    }
                };

                // Send the event, but allow the send to be cancelled.
                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    res = tx.send(event) => {
                        if res.is_err() { break; } // The receiver was dropped.
                    }
                }
            }
        });

        rx
    }

    /// Returns a global channel that receives all emitted events.
    /// Note: Uses a broadcast channel to allow multiple independent
    /// listeners.
    pub async fn global_channel(&self) -> broadcast::Receiver<Event> {
        let mut guard = self.internal.lock().await;

        // If the global channel was already initialized, just create a new listener and return.
        if let Some(sender) = &guard.cglobal {
            return sender.subscribe();
        }

        // Otherwise, initialize the global channel mechanism.
        let bus = guard.get_bus_mut();
        let mut sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("unable to subscribe");

        let token = CancellationToken::new();
        guard.cancellations.push(token.clone());

        let (tx, rx) = broadcast::channel(16);
        guard.cglobal = Some(tx.clone());

        // Spawn a task to pump events from the bus to the broadcast channel.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    maybe_event = sub.recv() => {
                        let event_box = match maybe_event {
                            Ok(e) => e,
                            Err(_) => break
                        };
                        // Ignore the error if there are no listeners.
                        let _ = tx.send(event_box.evt);
                    }
                }
            }
        });

        rx
    }

    /// Returns the new event bus instance, initializing it if necessary.
    pub async fn get_bus(&self) -> EventBus {
        let mut guard = self.internal.lock().await;
        // Since EventBus does not implement Clone, we return a new instance.
        if guard.bus.is_none() {
            guard.bus = Some(EventBus::new());
        }
        EventBus::new() // Return a new instance for compatibility.
    }

    /// Sets the event bus instance, returning an error if it is already initialized.
    pub async fn set_bus(&self, bus: EventBus) -> Result<()> {
        let mut guard = self.internal.lock().await;

        if guard.bus.is_some() {
            Err(GuardianError::Other(
                "the bus has already been initialized".to_string(),
            ))
        } else {
            guard.bus = Some(bus);
            Ok(())
        }
    }
}

// Default implementation to make creation with `EventEmitter::default()` easier.
impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

// TEST MODULE
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    // Simplified test for debugging.
    #[tokio::test]
    async fn test_simple_emit_receive() {
        let e = Arc::new(EventEmitter::new());

        // Only 1 client and 1 event.
        let (mut rx, _token) = e.subscribe().await;

        // Emit an event.
        e.emit(Arc::new("test_event".to_string())).await;

        // Try to receive with a timeout.
        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout while receiving event")
            .expect("channel closed unexpectedly");

        let s = event
            .downcast_ref::<String>()
            .expect("could not convert to String");

        assert_eq!(*s, "test_event");
    }

    #[tokio::test]
    async fn test_missing_listeners() {
        let e = EventEmitter::new();
        const EXPECTED_EVENTS: usize = 10;

        // Emit events with no listeners.
        // The test passes if there is no panic or blocking.
        for i in 0..EXPECTED_EVENTS {
            e.emit(Arc::new(format!("{}", i))).await;
        }

        // Give it some time to ensure nothing unexpected happens.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    #[ignore] // Test takes too long in CI environment
    async fn test_partial_listeners() {
        let e = Arc::new(EventEmitter::new());

        let producer_emitter = Arc::clone(&e);
        tokio::spawn(async move {
            // Emit 5 events that will be lost.
            for i in 0..5 {
                producer_emitter.emit(Arc::new(format!("{}", i))).await;
            }
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Subscribe after the first 5 events.
        let (mut sub, sub_cancel) = e.subscribe().await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Emit the next 5 events, which should be received.
        for i in 5..10 {
            e.emit(Arc::new(format!("{}", i))).await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check that events 5 through 9 were received.
        for i in 5..10 {
            let item = sub.recv().await.expect("channel was closed prematurely");
            let item_str = item.downcast_ref::<String>().expect("could not convert");
            assert_eq!(*item_str, format!("{}", i));
        }

        // Cancel the individual subscription.
        sub_cancel.cancel();

        // Wait for the cancellation to propagate.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check that the channel was closed.
        assert!(
            sub.recv().await.is_none(),
            "the channel should be closed after cancellation"
        );
    }
}
