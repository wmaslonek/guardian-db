pub mod base_store;
pub mod document_store;
pub mod event_log_store;
pub mod events;
pub mod kv_store;
pub mod operation;
// Replicator module removed - Iroh handles replication natively
// Only ReplicationInfo type remains for compatibility
pub mod replicator {
    pub mod replication_info;
}
