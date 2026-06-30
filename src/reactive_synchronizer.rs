//! # Reactive Synchronizer
//!
//! Reactive observability system for data synchronization between stores.
//! Allows external components (UI, logs, monitoring) to observe the state
//! of synchronization operations in real time.
//!
//! ## Components
//!
//! - **SyncObserver**: Observer that receives synchronization events
//! - **SyncProgress**: Current synchronization progress state
//! - **SyncEvent**: Events emitted during synchronization

use crate::address::Address;
use crate::guardian::error::{GuardianError, Result};
use crate::log::entry::Entry;
use crate::p2p::EventBus;
use crate::stores::events::{EventLoad, EventLoadProgress, EventReady, EventReplicated};
use iroh_blobs::Hash;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;

/// Synchronization progress state.
#[derive(Debug, Clone)]
pub struct SyncProgress {
    /// Address of the store being synchronized.
    pub address: Arc<dyn Address + Send + Sync>,
    /// Number of processed entries.
    pub processed: usize,
    /// Total number of entries to process.
    pub total: usize,
    /// Current state.
    pub state: SyncState,
}

impl SyncProgress {
    pub fn new(address: Arc<dyn Address + Send + Sync>) -> Self {
        Self {
            address,
            processed: 0,
            total: 0,
            state: SyncState::Idle,
        }
    }

    /// Computes the progress percentage (0-100).
    pub fn percentage(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.processed as f64 / self.total as f64) * 100.0
    }

    /// Checks whether synchronization is complete.
    pub fn is_complete(&self) -> bool {
        matches!(self.state, SyncState::Completed)
            || (self.total > 0 && self.processed >= self.total)
    }

    /// Checks whether it is in progress.
    pub fn is_active(&self) -> bool {
        matches!(self.state, SyncState::Loading | SyncState::Replicating)
    }
}

/// Possible synchronization states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Waiting to start.
    Idle,
    /// Loading data.
    Loading,
    /// Replicating to other peers.
    Replicating,
    /// Synchronization complete.
    Completed,
    /// Error during synchronization.
    Failed,
}

impl std::fmt::Display for SyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncState::Idle => write!(f, "Idle"),
            SyncState::Loading => write!(f, "Loading"),
            SyncState::Replicating => write!(f, "Replicating"),
            SyncState::Completed => write!(f, "Completed"),
            SyncState::Failed => write!(f, "Failed"),
        }
    }
}

/// Synchronization events.
#[derive(Debug, Clone)]
pub enum SyncEvent {
    /// Synchronization started.
    Started {
        address: Arc<dyn Address + Send + Sync>,
        total: usize,
    },
    /// Progress updated.
    Progress {
        address: Arc<dyn Address + Send + Sync>,
        hash: Hash,
        entry: Entry,
        processed: usize,
        total: usize,
    },
    /// Store ready.
    Ready {
        address: Arc<dyn Address + Send + Sync>,
        heads: Vec<Entry>,
    },
    /// Replication complete.
    Replicated {
        address: Arc<dyn Address + Send + Sync>,
        hash: Hash,
    },
    /// Error during synchronization.
    Error {
        address: Arc<dyn Address + Send + Sync>,
        error: String,
    },
}

impl SyncEvent {
    /// Returns the address associated with the event.
    pub fn address(&self) -> &Arc<dyn Address + Send + Sync> {
        match self {
            SyncEvent::Started { address, .. } => address,
            SyncEvent::Progress { address, .. } => address,
            SyncEvent::Ready { address, .. } => address,
            SyncEvent::Replicated { address, .. } => address,
            SyncEvent::Error { address, .. } => address,
        }
    }

    /// Returns the state corresponding to the event.
    pub fn state(&self) -> SyncState {
        match self {
            SyncEvent::Started { .. } => SyncState::Loading,
            SyncEvent::Progress { .. } => SyncState::Loading,
            SyncEvent::Ready { .. } => SyncState::Completed,
            SyncEvent::Replicated { .. } => SyncState::Replicating,
            SyncEvent::Error { .. } => SyncState::Failed,
        }
    }
}

/// Synchronization observer.
///
/// Allows external components to observe synchronization progress
/// through an event receiver.
///
/// # Example
///
/// ```rust,no_run
/// use guardian_db::reactive_synchronizer::{SyncObserver, SyncEvent};
///
/// async fn monitor_sync(observer: SyncObserver) {
///     let mut receiver = observer.subscribe().await.unwrap();
///
///     while let Ok(event) = receiver.recv().await {
///         match event {
///             SyncEvent::Started { total, .. } => {
///                 println!("Synchronization started: {} entries", total);
///             }
///             SyncEvent::Progress { processed, total, .. } => {
///                 let pct = (processed as f64 / total as f64) * 100.0;
///                 println!("Progress: {:.1}% ({}/{})", pct, processed, total);
///             }
///             SyncEvent::Ready { .. } => {
///                 println!("Synchronization complete!");
///             }
///             SyncEvent::Error { error, .. } => {
///                 eprintln!("Error: {}", error);
///             }
///             _ => {}
///         }
///     }
/// }
/// ```
pub struct SyncObserver {
    event_bus: Arc<EventBus>,
    progress: Arc<tokio::sync::RwLock<SyncProgress>>,
}

impl SyncObserver {
    /// Creates a new synchronization observer.
    pub fn new(event_bus: Arc<EventBus>, address: Arc<dyn Address + Send + Sync>) -> Self {
        Self {
            event_bus,
            progress: Arc::new(tokio::sync::RwLock::new(SyncProgress::new(address))),
        }
    }

    /// Subscribes to receive synchronization events.
    pub async fn subscribe(&self) -> Result<broadcast::Receiver<SyncEvent>> {
        self.event_bus.subscribe::<SyncEvent>().await
    }

    /// Returns the current progress.
    pub async fn current_progress(&self) -> SyncProgress {
        self.progress.read().await.clone()
    }

    /// Waits until synchronization is complete.
    pub async fn wait_for_completion(&self) -> Result<()> {
        let mut receiver = self.subscribe().await?;

        while let Ok(event) = receiver.recv().await {
            if matches!(event, SyncEvent::Ready { .. } | SyncEvent::Error { .. }) {
                break;
            }
        }

        let progress = self.current_progress().await;
        if matches!(progress.state, SyncState::Failed) {
            return Err(GuardianError::Store("Synchronization failed".to_string()));
        }

        Ok(())
    }

    /// Updates the internal progress (called by the event emitters).
    pub(crate) async fn update_progress<F>(&self, updater: F)
    where
        F: FnOnce(&mut SyncProgress),
    {
        let mut progress = self.progress.write().await;
        updater(&mut progress);
    }

    /// Emits a synchronization-start event.
    pub async fn emit_started(&self, total: usize) {
        self.update_progress(|p| {
            p.total = total;
            p.processed = 0;
            p.state = SyncState::Loading;
        })
        .await;

        let address = self.progress.read().await.address.clone();
        let event = SyncEvent::Started { address, total };

        if let Ok(emitter) = self.event_bus.emitter::<SyncEvent>().await
            && let Err(e) = emitter.emit(event)
        {
            warn!("Failed to emit SyncEvent::Started: {}", e);
        }
    }

    /// Emits a progress event.
    pub async fn emit_progress(&self, hash: Hash, entry: Entry, processed: usize, total: usize) {
        self.update_progress(|p| {
            p.processed = processed;
            p.total = total;
            p.state = SyncState::Loading;
        })
        .await;

        let address = self.progress.read().await.address.clone();
        let event = SyncEvent::Progress {
            address,
            hash,
            entry,
            processed,
            total,
        };

        if let Ok(emitter) = self.event_bus.emitter::<SyncEvent>().await
            && let Err(e) = emitter.emit(event)
        {
            warn!("Failed to emit SyncEvent::Progress: {}", e);
        }
    }

    /// Emits a replication-complete event.
    pub async fn emit_replicated(&self, hash: Hash) {
        self.update_progress(|p| {
            p.state = SyncState::Replicating;
        })
        .await;

        let address = self.progress.read().await.address.clone();
        let event = SyncEvent::Replicated { address, hash };

        if let Ok(emitter) = self.event_bus.emitter::<SyncEvent>().await
            && let Err(e) = emitter.emit(event)
        {
            warn!("Failed to emit SyncEvent::Replicated: {}", e);
        }
    }

    /// Emits a store-ready event.
    pub async fn emit_ready(&self, heads: Vec<Entry>) {
        self.update_progress(|p| {
            p.state = SyncState::Completed;
        })
        .await;

        let address = self.progress.read().await.address.clone();
        let event = SyncEvent::Ready { address, heads };

        if let Ok(emitter) = self.event_bus.emitter::<SyncEvent>().await
            && let Err(e) = emitter.emit(event)
        {
            warn!("Failed to emit SyncEvent::Ready: {}", e);
        }
    }

    /// Emits an error event.
    pub async fn emit_error(&self, error: String) {
        self.update_progress(|p| {
            p.state = SyncState::Failed;
        })
        .await;

        let address = self.progress.read().await.address.clone();
        let event = SyncEvent::Error { address, error };

        if let Ok(emitter) = self.event_bus.emitter::<SyncEvent>().await
            && let Err(e) = emitter.emit(event)
        {
            warn!("Failed to emit SyncEvent::Error: {}", e);
        }
    }
}

/// Converter from the old event system to the new one.
impl From<EventLoad> for SyncEvent {
    fn from(event: EventLoad) -> Self {
        SyncEvent::Started {
            address: event.address,
            total: event.heads.len(),
        }
    }
}

impl From<EventLoadProgress> for SyncEvent {
    fn from(event: EventLoadProgress) -> Self {
        SyncEvent::Progress {
            address: event.address,
            hash: event.hash,
            entry: event.entry,
            processed: event.progress as usize,
            total: event.max as usize,
        }
    }
}

impl From<EventReady> for SyncEvent {
    fn from(event: EventReady) -> Self {
        SyncEvent::Ready {
            address: event.address,
            heads: event.heads,
        }
    }
}

impl From<EventReplicated> for SyncEvent {
    fn from(event: EventReplicated) -> Self {
        // EventReplicated has no single hash, but rather multiple entries.
        // We use the hash of the first entry as representative.
        let hash = event
            .entries
            .first()
            .map(|e| *e.hash())
            .unwrap_or_else(|| iroh_blobs::Hash::from([0u8; 32]));

        SyncEvent::Replicated {
            address: event.address,
            hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_progress_percentage() {
        let address = Arc::new(crate::address::GuardianDBAddress::new(
            iroh_blobs::Hash::from([0u8; 32]),
            "test".to_string(),
        ));

        let mut progress = SyncProgress::new(address);
        assert_eq!(progress.percentage(), 0.0);

        progress.total = 100;
        progress.processed = 50;
        assert_eq!(progress.percentage(), 50.0);

        progress.processed = 100;
        assert_eq!(progress.percentage(), 100.0);
    }

    #[test]
    fn test_sync_progress_completion() {
        let address = Arc::new(crate::address::GuardianDBAddress::new(
            iroh_blobs::Hash::from([0u8; 32]),
            "test".to_string(),
        ));

        let mut progress = SyncProgress::new(address);
        assert!(!progress.is_complete());

        progress.state = SyncState::Completed;
        assert!(progress.is_complete());
    }

    #[test]
    fn test_sync_state_display() {
        assert_eq!(SyncState::Idle.to_string(), "Idle");
        assert_eq!(SyncState::Loading.to_string(), "Loading");
        assert_eq!(SyncState::Completed.to_string(), "Completed");
    }
}
