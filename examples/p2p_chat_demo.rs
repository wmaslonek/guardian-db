//! ╔════════════════════════════════════════════════════════════════════════════════╗
//! ║  ⚠ DEPRECATED | Bug fixes and improvements are being applied to the CHAT TUI. ║
//! ╚════════════════════════════════════════════════════════════════════════════════╝
//! P2P Chat Demo using GuardianDB - Advanced Version
//!
//! Features:
//! - Contact management (add, list, chat)
//! - Username linked to NodeID with persistence
//! - Private conversations (DM) per contact with separate logs
//! - Automatic message updates via P2P sync
//! - Full persistence (profile, contacts, conversation history)
//! - Contact status (online/offline/recently seen)
//! - File sending and receiving via /file <path>
//!
//! Run multiple chat instances to test P2P communication:
//! ```bash
//! cargo run --example p2p_chat_demo
//! ```
//!
//! # GuardianDB Features Used in This Demo
//!
//! This demo showcases several core guardian-db capabilities to build a fully
//! decentralized, peer-to-peer chat application with no central server:
//!
//! ## 1. IrohClient & Network Configuration (`ClientConfig`, `GossipConfig`)
//! The Iroh-based networking layer provides the P2P transport. `ClientConfig`
//! enables mDNS for automatic local peer discovery, n0 discovery for global
//! reachability, and pubsub/gossip for real-time message propagation.
//!
//! ## 2. GuardianDB Instance (`GuardianDB`, `NewGuardianDBOptions`)
//! The main database object that manages all event logs and the Iroh backend.
//! Each chat user gets their own GuardianDB instance (separate data directory),
//! which owns the event logs and coordinates P2P replication.
//!
//! ## 3. EventLogStore (append-only logs via `BaseGuardianDB::log()`)
//! Each conversation (DM or group) is backed by a named `EventLogStore`.
//! Messages are serialized to bytes and appended with `log.add()`. The log name
//! is deterministic (e.g., `dm-<shortA>-<shortB>`) so both peers produce the
//! same log name, enabling bilateral sync. `log.list()` retrieves all entries.
//! `log.load()` restores persisted entries from the sled cache on startup.
//!
//! ## 4. CreateDBOptions (log creation configuration)
//! When creating an event log, `CreateDBOptions` specifies:
//! - `create: true` — create the log if it doesn't exist
//! - `store_type: "eventlog"` — use the append-only event log type
//! - `replicate: true` — enable P2P replication for this log
//!
//! ## 5. EventLogStoreWrapper & `connect_to_peer()`
//! The `EventLogStoreWrapper` is obtained via `log.as_any().downcast_ref()`
//! and exposes `connect_to_peer(peer_id)`, which initiates a direct P2P
//! connection to exchange event log heads and synchronize entries. This is
//! the core mechanism for delivering messages between peers.
//!
//! ## 6. EventBus & EventExchangeHeads (reactive sync notifications)
//! `db.event_bus()` returns the global event bus. Subscribing to
//! `EventExchangeHeads` provides a stream of notifications whenever a peer
//! exchanges log heads, indicating new data is available. The chat uses this
//! to trigger UI updates and mark contacts as "online" in real time.
//!
//! ## 7. GuardianError & Result
//! The unified error type used across all guardian-db operations, propagated
//! throughout the chat for consistent error handling.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use dialoguer::{Input, Select, theme::ColorfulTheme};
// ─── GuardianDB imports ───
// These imports bring in all the guardian-db building blocks used by this chat:
use guardian_db::{
    guardian::{
        // EventLogStoreWrapper: concrete wrapper around EventLogStore that exposes
        // `connect_to_peer()` for initiating direct P2P sync with a specific node.
        EventLogStoreWrapper,
        // GuardianDB: the main database instance that owns event logs and the Iroh Backend.
        GuardianDB,
        core::{
            // EventExchangeHeads: event fired on the EventBus when a peer exchanges
            // log heads during P2P sync — used to detect incoming messages in real time.
            EventExchangeHeads,
            // NewGuardianDBOptions: configuration for creating a GuardianDB instance,
            // including the data directory and the Iroh backend reference.
            NewGuardianDBOptions,
        },
        // GuardianError/Result: unified error type for all guardian-db operations.
        error::{GuardianError, Result},
    },
    p2p::network::{
        // IrohClient: the P2P networking client built on the Iroh protocol.
        // Provides node identity (NodeId), peer discovery, and gossip transport.
        client::IrohClient,
        // ClientConfig: top-level network configuration (mDNS, n0 discovery, pubsub).
        // GossipConfig: gossip-layer settings like max message size for file transfers.
        config::{ClientConfig, GossipConfig},
    },
    traits::{
        // BaseGuardianDB: trait providing `.log()` to create/open named event logs.
        BaseGuardianDB,
        // CreateDBOptions: options passed to `.log()` — store type, replication, etc.
        CreateDBOptions,
        // EventLogStore: the append-only log trait with `.add()`, `.list()`, `.load()`.
        EventLogStore,
    },
};
use iroh::NodeId;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::Mutex,
    time::{Duration, sleep},
};
use tracing::{debug, error, info};

// ═══════════════════════════════════════════════════════════
// Data Structures
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserProfile {
    username: String,
    node_id: String,
    created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Contact {
    node_id: String,
    display_name: String,
    added_at: u64,
    #[serde(default)]
    last_seen: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContactsFile {
    contacts: Vec<Contact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Group {
    id: String,
    name: String,
    creator_node_id: String,
    members: Vec<GroupMember>,
    created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupMember {
    node_id: String,
    display_name: String,
    added_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupsFile {
    groups: Vec<Group>,
}

impl Group {
    fn generate_id(name: &str, creator: &str, ts: u64) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        name.hash(&mut h);
        creator.hash(&mut h);
        ts.hash(&mut h);
        format!("{:016x}", h.finish())
    }

    fn log_name(&self) -> String {
        format!("grp-{}", &self.id[..self.id.len().min(16)])
    }
}

/// File size limit: 5 MB
const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileAttachment {
    file_name: String,
    file_size: u64,
    file_data: String, // base64-encoded
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    from_node_id: String,
    from_username: String,
    content: String,
    timestamp: u64,
    #[serde(default = "default_message_type")]
    message_type: String,
    /// Identifies the target log: "dm-..." or "grp-..."
    /// Used to prevent mixing DM and group messages.
    #[serde(default)]
    target: String,
}

fn default_message_type() -> String {
    "text".to_string()
}

impl ChatMessage {
    fn new(from_node_id: String, from_username: String, content: String, target: String) -> Self {
        Self {
            from_node_id,
            from_username,
            content,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            message_type: "text".to_string(),
            target,
        }
    }

    fn new_file(
        from_node_id: String,
        from_username: String,
        attachment: &FileAttachment,
        target: String,
    ) -> Self {
        Self {
            from_node_id,
            from_username,
            content: serde_json::to_string(attachment).unwrap(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            message_type: "file".to_string(),
            target,
        }
    }

    fn new_group_invite(
        from_node_id: String,
        from_username: String,
        group: &Group,
        target: String,
    ) -> Self {
        Self {
            from_node_id,
            from_username,
            content: serde_json::to_string(group).unwrap(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            message_type: "group_invite".to_string(),
            target,
        }
    }

    fn is_text(&self) -> bool {
        self.message_type.is_empty() || self.message_type == "text"
    }

    fn is_file(&self) -> bool {
        self.message_type == "file"
    }

    fn file_attachment(&self) -> Option<FileAttachment> {
        if self.is_file() {
            serde_json::from_str(&self.content).ok()
        } else {
            None
        }
    }

    fn is_displayable(&self) -> bool {
        self.is_text() || self.is_file()
    }

    /// Checks if the message belongs to a DM log (or is legacy without target)
    fn belongs_to_dm(&self) -> bool {
        self.target.is_empty() || self.target.starts_with("dm-")
    }

    /// Checks if the message belongs to a group log
    fn belongs_to_group(&self, group_log_name: &str) -> bool {
        self.target == group_log_name
    }

    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }

    fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }

    fn display(&self) -> String {
        let datetime =
            chrono::DateTime::<chrono::Utc>::from(UNIX_EPOCH + Duration::from_secs(self.timestamp));
        let time_str = datetime.format("%H:%M:%S");
        if self.is_file() {
            if let Some(att) = self.file_attachment() {
                format!(
                    "[{}] {}: {} {} ({})",
                    time_str,
                    console::style(&self.from_username).cyan().bold(),
                    console::style("📎").bold(),
                    console::style(&att.file_name).yellow().bold(),
                    format_file_size(att.file_size)
                )
            } else {
                format!(
                    "[{}] {}: [corrupted file]",
                    time_str,
                    console::style(&self.from_username).cyan().bold(),
                )
            }
        } else {
            format!(
                "[{}] {}: {}",
                time_str,
                console::style(&self.from_username).cyan().bold(),
                &self.content
            )
        }
    }
}

// ═══════════════════════════════════════════════════════════
// Persistence
// ═══════════════════════════════════════════════════════════

fn profiles_dir() -> PathBuf {
    PathBuf::from("./chat_profiles")
}

fn profile_path(username: &str) -> PathBuf {
    profiles_dir().join(username).join("profile.json")
}

fn contacts_path(username: &str) -> PathBuf {
    profiles_dir().join(username).join("contacts.json")
}

fn load_profile(username: &str) -> Option<UserProfile> {
    let data = fs::read_to_string(profile_path(username)).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_profile(profile: &UserProfile) {
    let path = profile_path(&profile.username);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    if let Ok(data) = serde_json::to_string_pretty(profile) {
        fs::write(&path, data).ok();
    }
}

fn load_contacts(username: &str) -> Vec<Contact> {
    let data = match fs::read_to_string(contacts_path(username)) {
        Ok(d) => d,
        Err(_) => return vec![],
    };
    serde_json::from_str::<ContactsFile>(&data)
        .map(|f| f.contacts)
        .unwrap_or_default()
}

fn save_contacts(username: &str, contacts: &[Contact]) {
    let path = contacts_path(username);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let file = ContactsFile {
        contacts: contacts.to_vec(),
    };
    if let Ok(data) = serde_json::to_string_pretty(&file) {
        fs::write(&path, data).ok();
    }
}

fn find_existing_profiles() -> Vec<String> {
    let dir = profiles_dir();
    if !dir.exists() {
        return vec![];
    }
    let mut profiles = vec![];
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.path().join("profile.json").exists() {
                profiles.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    profiles
}

// ─── Pending sync persistence ───

fn pending_sync_path(username: &str) -> PathBuf {
    profiles_dir().join(username).join("pending_sync.json")
}

fn load_pending_sync(username: &str) -> HashSet<String> {
    let data = match fs::read_to_string(pending_sync_path(username)) {
        Ok(d) => d,
        Err(_) => return HashSet::new(),
    };
    serde_json::from_str::<Vec<String>>(&data)
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn save_pending_sync(username: &str, pending: &HashSet<String>) {
    let path = pending_sync_path(username);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let list: Vec<&String> = pending.iter().collect();
    if let Ok(data) = serde_json::to_string_pretty(&list) {
        fs::write(&path, data).ok();
    }
}

// ─── Group persistence ───

fn groups_path(username: &str) -> PathBuf {
    profiles_dir().join(username).join("groups.json")
}

// ─── Saved files tracking ───

fn saved_files_path(username: &str) -> PathBuf {
    profiles_dir().join(username).join("saved_files.json")
}

fn load_saved_files(username: &str) -> HashSet<String> {
    let data = match fs::read_to_string(saved_files_path(username)) {
        Ok(d) => d,
        Err(_) => return HashSet::new(),
    };
    serde_json::from_str::<Vec<String>>(&data)
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn save_saved_files(username: &str, saved: &HashSet<String>) {
    let path = saved_files_path(username);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let list: Vec<&String> = saved.iter().collect();
    if let Ok(data) = serde_json::to_string_pretty(&list) {
        fs::write(&path, data).ok();
    }
}

/// Generates a unique key for a received file, used to avoid re-saving.
fn file_save_key(msg: &ChatMessage, file_name: &str) -> String {
    format!("{}_{}_{}", msg.timestamp, msg.from_node_id, file_name)
}

fn load_groups(username: &str) -> Vec<Group> {
    let data = match fs::read_to_string(groups_path(username)) {
        Ok(d) => d,
        Err(_) => return vec![],
    };
    serde_json::from_str::<GroupsFile>(&data)
        .map(|f| f.groups)
        .unwrap_or_default()
}

fn save_groups(username: &str, groups: &[Group]) {
    let path = groups_path(username);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let file = GroupsFile {
        groups: groups.to_vec(),
    };
    if let Ok(data) = serde_json::to_string_pretty(&file) {
        fs::write(&path, data).ok();
    }
}

// ═══════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Displays a progress bar in the terminal (compatible with raw mode).
/// `stage` goes from 0 to `total` (inclusive).
fn print_progress_bar(stage: usize, total: usize, label: &str) {
    let bar_width = 20;
    let filled = if total > 0 {
        (stage * bar_width) / total
    } else {
        0
    };
    let empty = bar_width - filled;
    let pct = if total > 0 { (stage * 100) / total } else { 0 };
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
    // Clears the line and rewrites
    print!("\r{}\r", " ".repeat(80));
    print!(
        "  {} [{}] {}% - {}",
        if stage >= total {
            console::style("✓").green().to_string()
        } else {
            "⏳".to_string()
        },
        console::style(&bar).cyan(),
        pct,
        label
    );
    std::io::stdout().flush().ok();
}

/// Finishes the progress bar and moves to the next line.
fn finish_progress_bar() {
    print!("\r\n");
    std::io::stdout().flush().ok();
}

/// Opens the folder in the system's file manager.
fn open_folder(path: &std::path::Path) {
    let abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&abs_path)
            .spawn()
            .ok();
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&abs_path)
            .spawn()
            .ok();
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&abs_path)
            .spawn()
            .ok();
    }
}

/// Prepares a FileAttachment from a path on disk.
/// Reads the file, validates size, and encodes in base64.
fn prepare_file_attachment(file_path: &str) -> Result<(FileAttachment, String)> {
    let path = std::path::Path::new(file_path);
    if !path.exists() {
        return Err(GuardianError::Store(format!(
            "File not found: {}",
            file_path
        )));
    }
    let metadata = fs::metadata(path)
        .map_err(|e| GuardianError::Store(format!("Error reading metadata: {}", e)))?;
    if metadata.len() > MAX_FILE_SIZE {
        return Err(GuardianError::Store(format!(
            "File too large ({} > {} limit)",
            format_file_size(metadata.len()),
            format_file_size(MAX_FILE_SIZE)
        )));
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let data =
        fs::read(path).map_err(|e| GuardianError::Store(format!("Error reading file: {}", e)))?;
    let encoded = BASE64.encode(&data);
    let attachment = FileAttachment {
        file_name: file_name.clone(),
        file_size: metadata.len(),
        file_data: encoded,
    };
    Ok((attachment, file_name))
}

/// Saves a received file with a visible progress bar.
fn save_received_file_with_progress(
    username: &str,
    attachment: &FileAttachment,
    raw_mode: bool,
) -> Option<PathBuf> {
    let size_str = format_file_size(attachment.file_size);

    if raw_mode {
        print!("\r{}\r", " ".repeat(80));
        print!(
            "  {} {}\r\n",
            console::style("📥 Receiving file:").yellow().bold(),
            console::style(format!("{} ({})", &attachment.file_name, size_str)).cyan(),
        );
        std::io::stdout().flush().ok();
        print_progress_bar(0, 4, "Preparing...");
        std::thread::sleep(std::time::Duration::from_millis(150));
    } else {
        println!(
            "  {} {}",
            console::style("📥 Receiving file:").yellow().bold(),
            console::style(format!("{} ({})", &attachment.file_name, size_str)).cyan(),
        );
    }

    let dir = received_files_dir(username);
    fs::create_dir_all(&dir).ok()?;

    let safe_name = std::path::Path::new(&attachment.file_name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let mut path = dir.join(&safe_name);

    if path.exists() {
        let stem = std::path::Path::new(&safe_name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let ext = std::path::Path::new(&safe_name)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let mut counter = 1u32;
        loop {
            path = dir.join(format!("{}_{}{}", stem, counter, ext));
            if !path.exists() {
                break;
            }
            counter += 1;
        }
    }

    if raw_mode {
        print_progress_bar(1, 4, "Decoding base64...");
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    let data = BASE64.decode(&attachment.file_data).ok()?;

    if raw_mode {
        print_progress_bar(2, 4, &format!("Saving {} ...", size_str));
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    fs::write(&path, data).ok()?;

    if raw_mode {
        print_progress_bar(3, 4, "Verifying...");
        std::thread::sleep(std::time::Duration::from_millis(100));
        print_progress_bar(4, 4, &format!("📥 {} saved!", safe_name));
        finish_progress_bar();
        print!(
            "    {} Saved at: {}\r\n",
            console::style("💾").dim(),
            console::style(path.display()).dim(),
        );
        print!(
            "    {} Type {} to open the downloads folder\r\n",
            console::style("💡").dim(),
            console::style("/open").cyan(),
        );
        std::io::stdout().flush().ok();
    } else {
        println!(
            "    {} Saved at: {}",
            console::style("💾").dim(),
            console::style(path.display()).dim(),
        );
        println!(
            "    {} Type {} to open the downloads folder",
            console::style("💡").dim(),
            console::style("/open").cyan(),
        );
    }

    Some(path)
}

fn received_files_dir(username: &str) -> PathBuf {
    profiles_dir().join(username).join("received_files")
}

/// Generates a deterministic name for the DM log between two peers.
/// Both peers generate the same name because the IDs are sorted.
fn dm_log_name(node_a: &str, node_b: &str) -> String {
    let (first, second) = if node_a < node_b {
        (node_a, node_b)
    } else {
        (node_b, node_a)
    };
    let short_a = &first[..first.len().min(12)];
    let short_b = &second[..second.len().min(12)];
    format!("dm-{}-{}", short_a, short_b)
}

fn contact_status_display(last_seen: Option<u64>) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    match last_seen {
        Some(ts) => {
            let diff = now.saturating_sub(ts);
            if diff < 60 {
                format!("{}", console::style("● Online").green())
            } else if diff < 300 {
                format!("{}", console::style("● Recent").yellow())
            } else {
                let mins = diff / 60;
                if mins < 60 {
                    format!(
                        "{}",
                        console::style(format!("○ Seen {} min ago", mins)).dim()
                    )
                } else {
                    let hours = mins / 60;
                    format!("{}", console::style(format!("○ Seen {}h ago", hours)).dim())
                }
            }
        }
        None => format!("{}", console::style("○ Never seen").dim()),
    }
}

// ═══════════════════════════════════════════════════════════
// Chat State
// ═══════════════════════════════════════════════════════════

struct ChatState {
    db: Arc<GuardianDB>,
    profile: UserProfile,
    contacts: Vec<Contact>,
    groups: Vec<Group>,
    #[allow(clippy::type_complexity)]
    dm_logs: Arc<Mutex<HashMap<String, Arc<dyn EventLogStore<Error = GuardianError>>>>>,
    /// Messages persisted locally by log_name
    local_messages: HashMap<String, Vec<ChatMessage>>,
    #[allow(dead_code)]
    node_id: NodeId,
    has_new_messages: Arc<AtomicBool>,
    last_sync_peer: Arc<Mutex<Option<String>>>,
    pending_sync: Arc<Mutex<HashSet<String>>>,
    /// Tracks already saved files to avoid re-saving when re-entering the chat
    saved_files: HashSet<String>,
}

impl ChatState {
    async fn new(username: String) -> Result<Self> {
        // ── Step 1: Configure the Iroh P2P network layer ──
        // ClientConfig controls how this node participates in the P2P network:
        //   - enable_pubsub: enables the gossip publish/subscribe system for real-time
        //     message broadcasting between connected peers.
        //   - enable_discovery_mdns: activates mDNS-based local network discovery so
        //     peers on the same LAN find each other automatically without manual setup.
        //   - enable_discovery_n0: enables the n0 discovery service for finding peers
        //     across the internet (NAT traversal / relay).
        //   - data_store_path: directory where Iroh persists its internal state
        //     (node keys, connection metadata, etc.) — unique per chat user.
        //   - gossip.max_message_size: raised to 50MB to allow file transfers via
        //     the gossip layer (files are base64-encoded inside ChatMessage payloads).
        let config = ClientConfig {
            enable_pubsub: true,
            enable_discovery_mdns: true,
            enable_discovery_n0: true,
            data_store_path: Some(format!("./chat_data/{}", username).into()),
            gossip: GossipConfig {
                max_message_size: 50 * 1024 * 1024, // 50MB to support file transfers
                ..Default::default()
            },
            ..Default::default()
        };

        // ── Step 2: Create the IrohClient ──
        // IrohClient::new() starts the Iroh node with the given config. It generates
        // (or loads) a persistent Ed25519 keypair and returns a NodeId — the unique
        // cryptographic identity of this peer in the network.
        let client = IrohClient::new(config).await?;
        let node_id = client.node_id();

        info!("Chat started - Node ID: {}", node_id);

        let profile = match load_profile(&username) {
            Some(p) => {
                info!("Profile loaded: {}", p.username);
                p
            }
            None => {
                let p = UserProfile {
                    username: username.clone(),
                    node_id: node_id.to_string(),
                    created_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                };
                save_profile(&p);
                info!("New profile created: {}", p.username);
                p
            }
        };

        // ── Step 3: Create the GuardianDB instance ──
        // NewGuardianDBOptions configures the database:
        //   - directory: where GuardianDB stores its event logs on disk (sled backend).
        //     Each user gets a separate directory to isolate their data.
        //   - backend: a reference to the IrohClient's backend, which GuardianDB uses
        //     to create event logs that are automatically P2P-replicable.
        // GuardianDB::new() initializes the database, opens/creates the sled store,
        // and wires up the Iroh backend for P2P replication of all managed logs.
        let db_options = NewGuardianDBOptions {
            directory: Some(format!("./chat_db/{}", username).into()),
            backend: Some(client.backend().clone()),
            ..Default::default()
        };

        let db = Arc::new(GuardianDB::new(client, Some(db_options)).await?);
        let contacts = load_contacts(&username);
        let groups = load_groups(&username);

        Ok(Self {
            db,
            profile,
            contacts,
            groups,
            dm_logs: Arc::new(Mutex::new(HashMap::new())),
            local_messages: HashMap::new(),
            pending_sync: Arc::new(Mutex::new(load_pending_sync(&username))),
            node_id,
            has_new_messages: Arc::new(AtomicBool::new(false)),
            last_sync_peer: Arc::new(Mutex::new(None)),
            saved_files: load_saved_files(&username),
        })
    }

    /// Opens or creates the EventLogStore for a DM conversation.
    ///
    /// ## GuardianDB feature: EventLogStore via `BaseGuardianDB::log()`
    ///
    /// Each DM conversation is stored as a named, append-only event log. The log
    /// name is deterministic (`dm-<shortA>-<shortB>`, with IDs sorted) so both
    /// peers independently derive the same name and can sync the same log.
    ///
    /// Key operations used:
    /// - `db.log(name, options)` — creates or opens a named event log with the
    ///   given `CreateDBOptions`. The options specify `store_type: "eventlog"`
    ///   for append-only semantics and `replicate: true` to enable P2P sync.
    /// - `log.load(max)` — loads persisted entries from the sled on-disk cache
    ///   back into memory, restoring previous session data.
    /// - `log.add(bytes)` — appends a new entry (serialized ChatMessage).
    /// - `log.list(filter)` — retrieves all entries for reading/display.
    async fn get_or_create_dm_log(
        &mut self,
        contact_node_id: &str,
    ) -> Result<Arc<dyn EventLogStore<Error = GuardianError>>> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);

        // Cache: reuse already-opened logs to avoid sled lock conflicts.
        let mut dm_logs = self.dm_logs.lock().await;
        if let Some(log) = dm_logs.get(&log_name) {
            return Ok(log.clone());
        }

        // CreateDBOptions tells GuardianDB how to set up this log:
        //   - create: true  → create the log if it doesn't already exist
        //   - store_type: "eventlog" → use the append-only EventLogStore type
        //   - replicate: true → enable P2P replication so connected peers
        //     automatically exchange entries via the Iroh gossip layer
        let log_options = CreateDBOptions {
            create: Some(true),
            store_type: Some("eventlog".to_string()),
            replicate: Some(true),
            ..Default::default()
        };

        let log = self.db.log(&log_name, Some(log_options)).await?;

        // log.load() restores entries from the sled on-disk cache into memory,
        // so messages from previous sessions are available immediately.
        if let Err(e) = log.load(usize::MAX).await {
            debug!("Warning loading log history '{}': {}", log_name, e);
        }

        dm_logs.insert(log_name, log.clone());
        Ok(log)
    }

    /// Sends a text message to a contact via their shared DM event log.
    ///
    /// ## GuardianDB feature: `EventLogStore::add()`
    ///
    /// The message is serialized to bytes (JSON) and appended to the shared
    /// event log with `log.add()`. This makes it available for P2P sync —
    /// when `connect_to_peer()` is called later, the peer receives this entry.
    /// The contact is also marked as "pending sync" so the background retry
    /// task will attempt delivery even if the peer is currently offline.
    async fn send_dm(&mut self, contact_node_id: &str, content: String) -> Result<()> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);
        let msg = ChatMessage::new(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            content,
            log_name.clone(),
        );
        let log = self.get_or_create_dm_log(contact_node_id).await?;
        // EventLogStore::add() — appends the serialized message to the log.
        // This entry becomes part of the log's CRDT state and will be
        // replicated to the peer on the next connect_to_peer() call.
        log.add(msg.to_bytes()).await?;

        // Caches locally in memory
        let msgs = self.local_messages.entry(log_name.clone()).or_default();
        msgs.push(msg);

        // Marks contact as pending sync
        {
            let mut pending = self.pending_sync.lock().await;
            pending.insert(contact_node_id.to_string());
            save_pending_sync(&self.profile.username, &pending);
        }
        Ok(())
    }

    /// Adds a prepared FileAttachment to the DM log and persists locally.
    async fn store_file_dm(
        &mut self,
        contact_node_id: &str,
        attachment: &FileAttachment,
    ) -> Result<()> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);
        let msg = ChatMessage::new_file(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            attachment,
            log_name.clone(),
        );
        let log = self.get_or_create_dm_log(contact_node_id).await?;
        log.add(msg.to_bytes()).await?;
        let msgs = self.local_messages.entry(log_name.clone()).or_default();
        msgs.push(msg);
        {
            let mut pending = self.pending_sync.lock().await;
            pending.insert(contact_node_id.to_string());
            save_pending_sync(&self.profile.username, &pending);
        }
        Ok(())
    }

    /// Adds a prepared FileAttachment to the group log.
    async fn store_file_group(&mut self, group: &Group, attachment: &FileAttachment) -> Result<()> {
        let log_name = group.log_name();
        let msg = ChatMessage::new_file(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            attachment,
            log_name.clone(),
        );
        let log = self.get_or_create_group_log(group).await?;
        log.add(msg.to_bytes()).await?;
        let msgs = self.local_messages.entry(log_name.clone()).or_default();
        msgs.push(msg);
        Ok(())
    }

    /// Retrieves DM messages from the EventLogStore.
    ///
    /// ## GuardianDB feature: `EventLogStore::list()`
    ///
    /// `log.list(None)` returns all entries currently in the event log, including
    /// entries received from the remote peer via P2P sync. The store is the
    /// single source of truth — no JSON backup needed.
    async fn get_dm_messages(
        &mut self,
        contact_node_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);

        let log = self.get_or_create_dm_log(contact_node_id).await?;
        let entries = log.list(None).await?;
        let mut messages: Vec<ChatMessage> = entries
            .iter()
            .filter_map(|entry| ChatMessage::from_bytes(entry.value()))
            .filter(|m| m.belongs_to_dm())
            .collect();

        messages.sort_by_key(|m| m.timestamp);
        messages.dedup_by(|a, b| {
            a.timestamp == b.timestamp && a.from_node_id == b.from_node_id && a.content == b.content
        });

        // Update in-memory cache
        self.local_messages.insert(log_name, messages.clone());

        let start = messages.len().saturating_sub(limit);
        Ok(messages[start..].to_vec())
    }

    fn add_contact(&mut self, node_id: String, display_name: String) -> bool {
        if node_id == self.profile.node_id {
            return false;
        }
        if self.contacts.iter().any(|c| c.node_id == node_id) {
            return false;
        }
        self.contacts.push(Contact {
            node_id,
            display_name,
            added_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            last_seen: None,
        });
        save_contacts(&self.profile.username, &self.contacts);
        true
    }

    fn update_contact_last_seen(&mut self, node_id: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if let Some(c) = self.contacts.iter_mut().find(|c| c.node_id == node_id) {
            c.last_seen = Some(now);
            save_contacts(&self.profile.username, &self.contacts);
        }
    }

    /// Initiates a direct P2P connection to a contact to sync the DM event log.
    ///
    /// ## GuardianDB feature: `EventLogStoreWrapper::connect_to_peer()`
    ///
    /// This is the core P2P sync mechanism. The flow is:
    /// 1. The event log is downcast to `EventLogStoreWrapper` via `as_any()`.
    /// 2. `connect_to_peer(peer_id)` establishes a direct connection to the
    ///    target peer using the Iroh transport (with NAT traversal/relay).
    /// 3. Both sides exchange their log "heads" (latest entry hashes).
    /// 4. Missing entries are transferred in both directions (bidirectional sync).
    /// 5. After sync, both peers have an identical copy of the conversation.
    ///
    /// If the peer is offline, this returns an error, and the contact is kept
    /// in the "pending sync" set for background retry.
    async fn connect_to_contact(&mut self, contact_node_id: &str) -> Result<()> {
        let peer_id: NodeId = contact_node_id
            .parse()
            .map_err(|_| GuardianError::Store("Invalid NodeId".to_string()))?;

        let log = self.get_or_create_dm_log(contact_node_id).await?;

        // Downcast to EventLogStoreWrapper to access P2P-specific methods.
        // connect_to_peer() triggers bidirectional sync of the event log
        // with the remote peer, exchanging any entries they don't have.
        if let Some(wrapper) = log.as_any().downcast_ref::<EventLogStoreWrapper>() {
            wrapper.connect_to_peer(peer_id).await?;
            self.update_contact_last_seen(contact_node_id);
            {
                let mut pending = self.pending_sync.lock().await;
                if pending.remove(contact_node_id) {
                    save_pending_sync(&self.profile.username, &pending);
                }
            }
            info!("Connected to contact: {}", contact_node_id);
        } else {
            return Err(GuardianError::Store(
                "Could not access EventLogStoreWrapper".to_string(),
            ));
        }

        Ok(())
    }

    async fn send_raw_dm(&mut self, contact_node_id: &str, msg: ChatMessage) -> Result<()> {
        let log = self.get_or_create_dm_log(contact_node_id).await?;
        log.add(msg.to_bytes()).await?;
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);
        let msgs = self.local_messages.entry(log_name.clone()).or_default();
        msgs.push(msg);
        {
            let mut pending = self.pending_sync.lock().await;
            pending.insert(contact_node_id.to_string());
            save_pending_sync(&self.profile.username, &pending);
        }
        Ok(())
    }

    async fn send_group_invite(&mut self, contact_node_id: &str, group: &Group) -> Result<()> {
        let dm_target = dm_log_name(&self.profile.node_id, contact_node_id);
        let invite = ChatMessage::new_group_invite(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            group,
            dm_target,
        );
        self.send_raw_dm(contact_node_id, invite).await
    }

    fn process_group_invites(&mut self, messages: &[ChatMessage]) {
        let mut changed = false;
        for msg in messages {
            if msg.message_type == "group_invite"
                && let Ok(incoming) = serde_json::from_str::<Group>(&msg.content)
            {
                if let Some(existing) = self.groups.iter_mut().find(|g| g.id == incoming.id) {
                    for m in &incoming.members {
                        if !existing.members.iter().any(|em| em.node_id == m.node_id) {
                            existing.members.push(m.clone());
                            changed = true;
                        }
                    }
                } else {
                    self.groups.push(incoming);
                    changed = true;
                }
            }
        }
        if changed {
            save_groups(&self.profile.username, &self.groups);
        }
    }

    fn check_local_files_for_invites(&mut self) {
        let contact_ids: Vec<String> = self.contacts.iter().map(|c| c.node_id.clone()).collect();
        for contact_node_id in &contact_ids {
            let log_name = dm_log_name(&self.profile.node_id, contact_node_id);
            if let Some(msgs) = self.local_messages.get(&log_name) {
                let msgs_clone = msgs.clone();
                self.process_group_invites(&msgs_clone);
            }
        }
    }

    // ─── Group methods ───

    /// Opens or creates the EventLogStore for a group conversation.
    ///
    /// ## GuardianDB feature: shared multi-peer EventLogStore
    ///
    /// Group chats use the same EventLogStore mechanism as DMs, but the log is
    /// shared among multiple peers. The log name is derived from the group ID
    /// (`grp-<hash>`), and `replicate: true` ensures all members who connect
    /// to each other will sync the same log. Each member calls
    /// `connect_to_peer()` for every other member to form a mesh of sync
    /// connections, achieving eventual consistency across the group.
    async fn get_or_create_group_log(
        &mut self,
        group: &Group,
    ) -> Result<Arc<dyn EventLogStore<Error = GuardianError>>> {
        let log_name = group.log_name();

        let mut dm_logs = self.dm_logs.lock().await;
        if let Some(log) = dm_logs.get(&log_name) {
            return Ok(log.clone());
        }

        let log_options = CreateDBOptions {
            create: Some(true),
            store_type: Some("eventlog".to_string()),
            replicate: Some(true),
            ..Default::default()
        };

        let log = self.db.log(&log_name, Some(log_options)).await?;

        if let Err(e) = log.load(usize::MAX).await {
            debug!("Warning loading group history '{}': {}", log_name, e);
        }

        dm_logs.insert(log_name, log.clone());
        Ok(log)
    }

    async fn send_group_message(&mut self, group: &Group, content: String) -> Result<()> {
        let msg = ChatMessage::new(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            content,
            group.log_name(),
        );
        let log = self.get_or_create_group_log(group).await?;
        log.add(msg.to_bytes()).await?;

        let log_name = group.log_name();
        let msgs = self.local_messages.entry(log_name.clone()).or_default();
        msgs.push(msg);
        Ok(())
    }

    async fn get_group_messages(
        &mut self,
        group: &Group,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let log_name = group.log_name();

        let log = self.get_or_create_group_log(group).await?;
        let entries = log.list(None).await?;
        let mut messages: Vec<ChatMessage> = entries
            .iter()
            .filter_map(|entry| ChatMessage::from_bytes(entry.value()))
            .filter(|m| m.belongs_to_group(&log_name) || (m.target.is_empty() && m.is_text()))
            .collect();

        messages.sort_by_key(|m| m.timestamp);
        messages.dedup_by(|a, b| {
            a.timestamp == b.timestamp && a.from_node_id == b.from_node_id && a.content == b.content
        });

        // Update in-memory cache
        self.local_messages.insert(log_name, messages.clone());

        let start = messages.len().saturating_sub(limit);
        Ok(messages[start..].to_vec())
    }

    /// Syncs the group event log with all members via `connect_to_peer()`.
    ///
    /// ## GuardianDB feature: multi-peer EventLogStore sync
    ///
    /// For group chats, we iterate over all members and call `connect_to_peer()`
    /// on the shared group log for each one. This creates a mesh of P2P sync
    /// connections: each pair of members exchanges log heads and missing entries.
    /// The EventLogStore's CRDT-based replication ensures all members converge
    /// to the same message history regardless of connection order.
    async fn sync_group_members(&mut self, group: &Group) -> (usize, usize) {
        let log = match self.get_or_create_group_log(group).await {
            Ok(l) => l,
            Err(_) => return (0, group.members.len()),
        };
        let mut ok = 0;
        let mut fail = 0;
        for member in &group.members {
            if member.node_id == self.profile.node_id {
                continue;
            }
            if let Ok(peer_id) = member.node_id.parse::<NodeId>() {
                if let Some(wrapper) = log.as_any().downcast_ref::<EventLogStoreWrapper>() {
                    match wrapper.connect_to_peer(peer_id).await {
                        Ok(()) => {
                            self.update_contact_last_seen(&member.node_id);
                            ok += 1;
                        }
                        Err(_) => {
                            fail += 1;
                        }
                    }
                } else {
                    fail += 1;
                }
            } else {
                fail += 1;
            }
        }
        (ok, fail)
    }

    fn create_group(&mut self, name: String, member_indices: &[usize]) -> Group {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let id = Group::generate_id(&name, &self.profile.node_id, now);

        let mut members = vec![GroupMember {
            node_id: self.profile.node_id.clone(),
            display_name: self.profile.username.clone(),
            added_at: now,
        }];

        for &idx in member_indices {
            if idx < self.contacts.len() {
                let c = &self.contacts[idx];
                members.push(GroupMember {
                    node_id: c.node_id.clone(),
                    display_name: c.display_name.clone(),
                    added_at: now,
                });
            }
        }

        let group = Group {
            id,
            name,
            creator_node_id: self.profile.node_id.clone(),
            members,
            created_at: now,
        };

        self.groups.push(group.clone());
        save_groups(&self.profile.username, &self.groups);
        group
    }

    fn add_member_to_group(&mut self, group_idx: usize, contact_idx: usize) -> bool {
        let contact = &self.contacts[contact_idx];
        let group = &self.groups[group_idx];
        if group.members.iter().any(|m| m.node_id == contact.node_id) {
            return false;
        }
        let member = GroupMember {
            node_id: contact.node_id.clone(),
            display_name: contact.display_name.clone(),
            added_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        self.groups[group_idx].members.push(member);
        save_groups(&self.profile.username, &self.groups);
        true
    }

    fn display_welcome(&self) {
        println!("\n{}", console::style("═".repeat(60)).green());
        println!(
            "{}",
            console::style("  GuardianDB - Decentralized P2P Chat")
                .green()
                .bold()
        );
        println!("{}", console::style("═".repeat(60)).green());
        println!(
            "  User:    {}",
            console::style(&self.profile.username).cyan().bold()
        );
        println!(
            "  Node ID:  {}",
            console::style(&self.profile.node_id).yellow()
        );
        println!("  Contacts: {}", self.contacts.len());
        println!("  Groups:   {}", self.groups.len());
        println!("{}\n", console::style("═".repeat(60)).green());
    }
}

// ═══════════════════════════════════════════════════════════
// UI Flows
// ═══════════════════════════════════════════════════════════

fn select_or_create_profile(theme: &ColorfulTheme) -> std::result::Result<String, GuardianError> {
    let existing = find_existing_profiles();

    if existing.is_empty() {
        println!(
            "\n{}",
            console::style("Welcome to GuardianDB Chat!").green().bold()
        );
        let username: String = Input::with_theme(theme)
            .with_prompt("Choose a username")
            .interact_text()
            .map_err(|e| GuardianError::Store(format!("Error reading input: {}", e)))?;
        Ok(username)
    } else {
        let mut items: Vec<String> = existing.iter().map(|u| format!("👤 {}", u)).collect();
        items.push("➕ Create new profile".to_string());

        let sel = Select::with_theme(theme)
            .with_prompt("Select a profile or create a new one")
            .items(&items)
            .default(0)
            .interact()
            .map_err(|e| GuardianError::Store(format!("Error: {}", e)))?;

        if sel < existing.len() {
            Ok(existing[sel].clone())
        } else {
            let username: String = Input::with_theme(theme)
                .with_prompt("Choose a username")
                .interact_text()
                .map_err(|e| GuardianError::Store(format!("Error: {}", e)))?;
            Ok(username)
        }
    }
}

async fn add_contact_flow(state: &mut ChatState, theme: &ColorfulTheme) {
    println!("\n{}", console::style("═ Add Contact ═").green().bold());
    println!(
        "  Your Node ID: {}",
        console::style(&state.profile.node_id).yellow()
    );
    println!(
        "  {}",
        console::style("Share this Node ID with the person you want to add.").dim()
    );

    let node_id_str: String = Input::with_theme(theme)
        .with_prompt("Contact Node ID (empty to cancel)")
        .allow_empty(true)
        .interact_text()
        .unwrap();

    if node_id_str.is_empty() {
        return;
    }

    if node_id_str.parse::<NodeId>().is_err() {
        println!("  {}", console::style("✗ Invalid Node ID!").red().bold());
        return;
    }

    if node_id_str == state.profile.node_id {
        println!(
            "  {}",
            console::style("✗ You cannot add yourself as a contact!")
                .red()
                .bold()
        );
        return;
    }

    if state.contacts.iter().any(|c| c.node_id == node_id_str) {
        println!(
            "  {}",
            console::style("✗ Contact already exists!").yellow().bold()
        );
        return;
    }

    let display_name: String = Input::with_theme(theme)
        .with_prompt("Contact name")
        .interact_text()
        .unwrap();

    if state.add_contact(node_id_str.clone(), display_name.clone()) {
        println!(
            "  {}",
            console::style(format!("✓ Contact '{}' added!", display_name))
                .green()
                .bold()
        );

        println!("  ⏳ Trying to connect...");
        match state.connect_to_contact(&node_id_str).await {
            Ok(()) => {
                println!("  {}", console::style("✓ Connected and syncing!").green());
            }
            Err(e) => {
                println!(
                    "  {}",
                    console::style(format!("⚠ Connection unavailable now: {}", e)).yellow()
                );
                println!(
                    "  {}",
                    console::style("Connection will be retried when starting a conversation.")
                        .dim()
                );
            }
        }
    }
}

fn display_contacts(state: &ChatState) {
    println!("\n{}", console::style("═".repeat(60)).green());
    println!("{}", console::style("  My Contacts").green().bold());
    println!("{}", console::style("═".repeat(60)).green());

    if state.contacts.is_empty() {
        println!("  {}", console::style("No contacts added").dim());
        println!(
            "  {}",
            console::style("Use 'Add contact' to get started!").cyan()
        );
    } else {
        for (i, contact) in state.contacts.iter().enumerate() {
            let id_preview = &contact.node_id[..contact.node_id.len().min(16)];
            println!(
                "  {}. {} {} - {}",
                i + 1,
                console::style(&contact.display_name).cyan().bold(),
                contact_status_display(contact.last_seen),
                console::style(format!("ID: {}...", id_preview)).dim()
            );
        }
    }
    println!("{}\n", console::style("═".repeat(60)).green());
}

fn display_profile(state: &ChatState) {
    println!("\n{}", console::style("═".repeat(60)).green());
    println!("{}", console::style("  My Profile").green().bold());
    println!("{}", console::style("═".repeat(60)).green());
    println!(
        "  Name:     {}",
        console::style(&state.profile.username).cyan().bold()
    );
    println!(
        "  Node ID:  {}",
        console::style(&state.profile.node_id).yellow()
    );
    let created = chrono::DateTime::<chrono::Utc>::from(
        UNIX_EPOCH + Duration::from_secs(state.profile.created_at),
    );
    println!("  Created:  {}", created.format("%d/%m/%Y %H:%M"));
    println!("  Contacts: {}", state.contacts.len());
    println!("{}", console::style("═".repeat(60)).green());
    println!(
        "\n  {}",
        console::style("📋 Copy the Node ID above to share with other users.").dim()
    );
}

async fn create_group_flow(state: &mut ChatState, theme: &ColorfulTheme) {
    println!("\n{}", console::style("═ Create Group ═").green().bold());

    if state.contacts.is_empty() {
        println!(
            "  {}",
            console::style("Add contacts before creating a group.").yellow()
        );
        return;
    }

    let group_name: String = Input::with_theme(theme)
        .with_prompt("Group name (empty to cancel)")
        .allow_empty(true)
        .interact_text()
        .unwrap();

    if group_name.is_empty() {
        return;
    }

    println!(
        "  {}",
        console::style("Select members one by one. Choose '✅ Finish' when done.").dim()
    );

    let mut chosen: Vec<usize> = Vec::new();

    loop {
        let mut items: Vec<String> = state
            .contacts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mark = if chosen.contains(&i) { "✓ " } else { "  " };
                format!(
                    "{}{} ({}...)",
                    mark,
                    c.display_name,
                    &c.node_id[..c.node_id.len().min(8)]
                )
            })
            .collect();
        let done_label = format!("✅ Finish ({} selected)", chosen.len());
        items.push(done_label);
        items.push("❌ Cancel".to_string());

        let sel = Select::with_theme(theme)
            .with_prompt("Add/remove member")
            .items(&items)
            .default(0)
            .interact()
            .unwrap();

        if sel == items.len() - 1 {
            // Cancel
            return;
        } else if sel == items.len() - 2 {
            // Finish
            break;
        } else {
            // Toggle contact
            if let Some(pos) = chosen.iter().position(|&x| x == sel) {
                chosen.remove(pos);
                println!(
                    "  {} removed",
                    console::style(&state.contacts[sel].display_name).yellow()
                );
            } else {
                chosen.push(sel);
                println!(
                    "  {} added ✓",
                    console::style(&state.contacts[sel].display_name).green()
                );
            }
        }
    }

    if chosen.is_empty() {
        println!(
            "  {}",
            console::style("No members selected. Group not created.").yellow()
        );
        return;
    }

    let group = state.create_group(group_name.clone(), &chosen);
    println!(
        "  {}",
        console::style(format!(
            "✓ Group '{}' created with {} members!",
            group_name,
            group.members.len()
        ))
        .green()
        .bold()
    );

    // Sends invite to each member via DM and syncs immediately
    println!("  📨 Sending invites and syncing...");
    let mut ok = 0;
    let mut fail = 0;
    for member in &group.members {
        if member.node_id != state.profile.node_id {
            // Sends invite in the DM log
            if let Err(e) = state.send_group_invite(&member.node_id, &group).await {
                debug!("Error sending invite to {}: {}", member.display_name, e);
            }
            // Syncs the DM log to deliver the invite immediately
            match state.connect_to_contact(&member.node_id).await {
                Ok(()) => ok += 1,
                Err(_) => fail += 1,
            }
        }
    }

    // Also connects to the group log for future messages
    state.sync_group_members(&group).await;

    if ok > 0 {
        println!(
            "  {}",
            console::style(format!("✓ {} member(s) connected", ok)).green()
        );
    }
    if fail > 0 {
        println!(
            "  {}",
            console::style(format!("⚠ {} member(s) offline", fail)).yellow()
        );
    }
}

fn display_groups(state: &ChatState) {
    println!("\n{}", console::style("═".repeat(60)).green());
    println!("{}", console::style("  My Groups").green().bold());
    println!("{}", console::style("═".repeat(60)).green());

    if state.groups.is_empty() {
        println!("  {}", console::style("No groups created").dim());
        println!(
            "  {}",
            console::style("Use 'Create group' to get started!").cyan()
        );
    } else {
        for (i, group) in state.groups.iter().enumerate() {
            let member_names: Vec<&str> = group
                .members
                .iter()
                .map(|m| m.display_name.as_str())
                .collect();
            println!(
                "  {}. {} - {} member(s): {}",
                i + 1,
                console::style(&group.name).cyan().bold(),
                group.members.len(),
                console::style(member_names.join(", ")).dim()
            );
        }
    }
    println!("{}\n", console::style("═".repeat(60)).green());
}

async fn add_member_to_group_flow(state: &mut ChatState, theme: &ColorfulTheme) {
    if state.groups.is_empty() {
        println!(
            "\n  {}",
            console::style("No groups! Create a group first.").yellow()
        );
        return;
    }
    if state.contacts.is_empty() {
        println!(
            "\n  {}",
            console::style("No contacts! Add contacts first.").yellow()
        );
        return;
    }

    let group_names: Vec<String> = state
        .groups
        .iter()
        .map(|g| format!("{} ({} members)", g.name, g.members.len()))
        .collect();

    let mut items = group_names;
    items.push("↩ Back".to_string());

    let gsel = Select::with_theme(theme)
        .with_prompt("Select the group")
        .items(&items)
        .default(0)
        .interact()
        .unwrap();

    if gsel >= state.groups.len() {
        return;
    }

    let existing_members: HashSet<String> = state.groups[gsel]
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .collect();

    let available: Vec<(usize, &Contact)> = state
        .contacts
        .iter()
        .enumerate()
        .filter(|(_, c)| !existing_members.contains(&c.node_id))
        .collect();

    if available.is_empty() {
        println!(
            "\n  {}",
            console::style("All your contacts are already members of this group.").yellow()
        );
        return;
    }

    let contact_names: Vec<String> = available
        .iter()
        .map(|(_, c)| c.display_name.clone())
        .collect();

    let csel = Select::with_theme(theme)
        .with_prompt("Select the contact to add")
        .items(&contact_names)
        .default(0)
        .interact()
        .unwrap();

    let contact_idx = available[csel].0;
    let contact_name = state.contacts[contact_idx].display_name.clone();

    if state.add_member_to_group(gsel, contact_idx) {
        println!(
            "  {}",
            console::style(format!("✓ '{}' added to the group!", contact_name))
                .green()
                .bold()
        );

        // Sends updated group to all members and syncs
        let group = state.groups[gsel].clone();
        for member in &group.members {
            if member.node_id != state.profile.node_id {
                state.send_group_invite(&member.node_id, &group).await.ok();
                state.connect_to_contact(&member.node_id).await.ok();
            }
        }
        state.sync_group_members(&group).await;
    } else {
        println!(
            "  {}",
            console::style("✗ Member already exists in the group.").yellow()
        );
    }
}

async fn group_chat_mode(state: &mut ChatState, group_idx: usize, _theme: &ColorfulTheme) {
    let group = state.groups[group_idx].clone();

    // Tries to sync with group members
    println!("\n  ⏳ Connecting to group members...");
    let (ok, fail) = state.sync_group_members(&group).await;
    if ok > 0 {
        println!(
            "  {}",
            console::style(format!("✓ {} member(s) connected", ok)).green()
        );
    }
    if fail > 0 {
        println!(
            "  {}",
            console::style(format!("⚠ {} member(s) offline", fail)).yellow()
        );
    }

    // Header
    println!("\n{}", console::style("═".repeat(60)).green());
    println!(
        "  👥 Group: {} ({} members)",
        console::style(&group.name).cyan().bold(),
        group.members.len()
    );
    let member_names: Vec<&str> = group
        .members
        .iter()
        .map(|m| m.display_name.as_str())
        .collect();
    println!(
        "  {}",
        console::style(format!("Members: {}", member_names.join(", "))).dim()
    );
    println!("{}", console::style("═".repeat(60)).green());
    println!(
        "  {}",
        console::style("Commands: /back or Esc = go back | /sync = sync | /members = show members")
            .dim()
    );
    println!(
        "  {}",
        console::style("/file <path> = send file | /files = list received | /open = open folder")
            .dim()
    );
    println!("{}", console::style("─".repeat(60)).dim());

    // Shows recent messages
    let mut last_count: usize = 0;
    match state.get_group_messages(&group, 50).await {
        Ok(messages) => {
            if messages.is_empty() {
                println!(
                    "  {}",
                    console::style("Nenhuma mensagem. Envie a primeira!").dim()
                );
            } else {
                for msg in messages.iter().filter(|m| m.is_displayable()) {
                    println!("  {}", msg.display());
                    if msg.is_file()
                        && msg.from_node_id != state.profile.node_id
                        && let Some(att) = msg.file_attachment()
                    {
                        let key = file_save_key(msg, &att.file_name);
                        if !state.saved_files.contains(&key) {
                            save_received_file_with_progress(&state.profile.username, &att, false);
                            state.saved_files.insert(key);
                            save_saved_files(&state.profile.username, &state.saved_files);
                        }
                    }
                }
                last_count = messages.len();
            }
        }
        Err(e) => {
            println!(
                "  {} Error loading messages: {}",
                console::style("✗").red(),
                e
            );
        }
    }
    println!("{}", console::style("─".repeat(60)).dim());

    terminal::enable_raw_mode().ok();

    let mut input_buf = String::new();
    let mut prompt_shown = false;
    let mut needs_poll = false;
    let mut last_poll_time = Instant::now();

    loop {
        // Checks sync notifications
        if state.has_new_messages.swap(false, Ordering::Relaxed) {
            let peer_id = state.last_sync_peer.lock().await.take();
            if let Some(ref peer_id) = peer_id {
                state.update_contact_last_seen(peer_id);
            }
            needs_poll = true;
        }

        // Fallback: periodic poll every 2 seconds
        if !needs_poll && last_poll_time.elapsed() >= std::time::Duration::from_secs(2) {
            needs_poll = true;
        }

        // Checks for new messages (only when sync or timer occurred)
        if needs_poll {
            needs_poll = false;
            last_poll_time = Instant::now();
            if let Ok(messages) = state.get_group_messages(&group, 50).await
                && messages.len() > last_count
            {
                if prompt_shown {
                    print!("\r{}\r", " ".repeat(80));
                }
                for msg in messages[last_count..].iter().filter(|m| m.is_displayable()) {
                    print!("  {}\r\n", msg.display());
                    if msg.is_file()
                        && msg.from_node_id != state.profile.node_id
                        && let Some(att) = msg.file_attachment()
                    {
                        let key = file_save_key(msg, &att.file_name);
                        if !state.saved_files.contains(&key) {
                            save_received_file_with_progress(&state.profile.username, &att, true);
                            state.saved_files.insert(key.clone());
                            save_saved_files(&state.profile.username, &state.saved_files);
                        }
                    }
                }
                std::io::stdout().flush().ok();
                last_count = messages.len();
                prompt_shown = false;
            }
        } // end needs_poll

        if !prompt_shown {
            print!(
                "  {} ",
                console::style(format!("{}:", &state.profile.username))
                    .cyan()
                    .bold()
            );
            if !input_buf.is_empty() {
                print!("{}", &input_buf);
            }
            std::io::stdout().flush().ok();
            prompt_shown = true;
        }

        if event::poll(std::time::Duration::from_millis(200)).unwrap_or(false)
            && let Ok(Event::Key(key_ev)) = event::read()
        {
            if key_ev.kind != KeyEventKind::Press {
                continue;
            }
            match key_ev {
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    print!("\r\n");
                    std::io::stdout().flush().ok();
                    prompt_shown = false;

                    let text = input_buf.trim().to_string();
                    input_buf.clear();

                    match text.as_str() {
                        "/back" | "/sair" | "/exit" => break,
                        "/sync" => {
                            print!("  ⏳ Syncing with members...\r\n");
                            std::io::stdout().flush().ok();
                            let (ok, fail) = state.sync_group_members(&group).await;
                            print!(
                                "  {} connected, {} offline\r\n",
                                console::style(format!("✓ {}", ok)).green(),
                                fail
                            );
                            std::io::stdout().flush().ok();
                            needs_poll = true;
                        }
                        "/members" => {
                            print!("  Group members:\r\n");
                            for m in &group.members {
                                print!("    - {}\r\n", m.display_name);
                            }
                            std::io::stdout().flush().ok();
                        }
                        "/open" => {
                            let dir = received_files_dir(&state.profile.username);
                            fs::create_dir_all(&dir).ok();
                            print!("  📂 Opening: {}\r\n", dir.display());
                            std::io::stdout().flush().ok();
                            open_folder(&dir);
                        }
                        "/files" => {
                            let dir = received_files_dir(&state.profile.username);
                            if dir.exists() {
                                print!("  📁 Received files ({}):\r\n", dir.display());
                                if let Ok(entries) = fs::read_dir(&dir) {
                                    for entry in entries.flatten() {
                                        let meta = entry.metadata().ok();
                                        let size = meta
                                            .map(|m| format_file_size(m.len()))
                                            .unwrap_or_default();
                                        print!(
                                            "    - {} ({})\r\n",
                                            entry.file_name().to_string_lossy(),
                                            size
                                        );
                                    }
                                }
                            } else {
                                print!("  No files received yet.\r\n");
                            }
                            std::io::stdout().flush().ok();
                        }
                        "" => {}
                        msg if msg.starts_with("/file ") => {
                            let file_path = msg.strip_prefix("/file ").unwrap().trim();
                            if file_path.is_empty() {
                                print!(
                                    "  {} Usage: /file <file_path>\r\n",
                                    console::style("⚠").yellow()
                                );
                                std::io::stdout().flush().ok();
                            } else {
                                // Step 1/4: Reading file
                                print_progress_bar(0, 4, "Reading file...");
                                match prepare_file_attachment(file_path) {
                                    Ok((attachment, file_name)) => {
                                        let size_str = format_file_size(attachment.file_size);

                                        // Step 2/4: Encoded
                                        print_progress_bar(
                                            1,
                                            4,
                                            &format!("Encoding {} ({})...", file_name, size_str),
                                        );

                                        // Step 3/4: Saving to log
                                        print_progress_bar(2, 4, "Saving to P2P log...");
                                        match state.store_file_group(&group, &attachment).await {
                                            Ok(()) => {
                                                // Step 4/4: Syncing
                                                print_progress_bar(3, 4, "Syncing with members...");
                                                let (ok, _fail) =
                                                    state.sync_group_members(&group).await;

                                                // Concluído
                                                print_progress_bar(
                                                    4,
                                                    4,
                                                    &format!(
                                                        "📤 '{}' sent! ({}{})\r\n",
                                                        file_name,
                                                        size_str,
                                                        if ok > 0 {
                                                            format!(", {} peer(s) sync", ok)
                                                        } else {
                                                            String::new()
                                                        }
                                                    ),
                                                );
                                                finish_progress_bar();
                                                // Update last_count to prevent poll re-display
                                                if let Ok(messages) =
                                                    state.get_group_messages(&group, 50).await
                                                {
                                                    last_count = messages.len();
                                                }
                                            }
                                            Err(e) => {
                                                finish_progress_bar();
                                                print!(
                                                    "  {} Error saving: {}\r\n",
                                                    console::style("✗").red(),
                                                    e
                                                );
                                                std::io::stdout().flush().ok();
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        finish_progress_bar();
                                        print!("  {} Error: {}\r\n", console::style("✗").red(), e);
                                        std::io::stdout().flush().ok();
                                    }
                                }
                            }
                        }
                        msg => {
                            match state.send_group_message(&group, msg.to_string()).await {
                                Ok(()) => {
                                    // Replace the prompt line with the formatted message
                                    print!("\x1B[1A\r{}\r", " ".repeat(80));
                                    if let Ok(messages) = state.get_group_messages(&group, 50).await
                                    {
                                        if let Some(last_msg) = messages.last() {
                                            print!("  {}\r\n", last_msg.display());
                                        }
                                        last_count = messages.len();
                                    }
                                    std::io::stdout().flush().ok();
                                }
                                Err(e) => {
                                    print!(
                                        "  {} Error sending: {}\r\n",
                                        console::style("✗").red(),
                                        e
                                    );
                                    std::io::stdout().flush().ok();
                                }
                            }
                        }
                    }
                }
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => {
                    print!("\r\n");
                    std::io::stdout().flush().ok();
                    break;
                }
                KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                } => {
                    if input_buf.pop().is_some() {
                        print!("\x08 \x08");
                        std::io::stdout().flush().ok();
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {
                    print!("\r\n");
                    std::io::stdout().flush().ok();
                    break;
                }
                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => {
                    input_buf.push(c);
                    print!("{}", c);
                    std::io::stdout().flush().ok();
                }
                _ => {}
            }
        }
    }

    terminal::disable_raw_mode().ok();
}

async fn chat_mode(state: &mut ChatState, contact_idx: usize, _theme: &ColorfulTheme) {
    let contact = state.contacts[contact_idx].clone();

    // Tries to connect to the peer
    println!("\n  ⏳ Connecting to peer...");
    match state.connect_to_contact(&contact.node_id).await {
        Ok(()) => {
            println!("  {}", console::style("✓ Connected!").green());
        }
        Err(e) => {
            println!(
                "  {}",
                console::style(format!("⚠ Connection: {} (offline mode)", e)).yellow()
            );
        }
    }

    // Header
    println!("\n{}", console::style("═".repeat(60)).green());
    println!(
        "  💬 Chat with {} {}",
        console::style(&contact.display_name).cyan().bold(),
        contact_status_display(contact.last_seen)
    );
    println!("{}", console::style("═".repeat(60)).green());
    println!(
        "  {}",
        console::style("Commands: /back or Esc = go back | /sync = sync").dim()
    );
    println!(
        "  {}",
        console::style("/file <path> = send file | /files = list | /open = open folder").dim()
    );
    println!(
        "  {}",
        console::style("Messages update automatically.").dim()
    );
    println!("{}", console::style("─".repeat(60)).dim());

    // Shows recent messages
    let mut last_count: usize = 0;
    match state.get_dm_messages(&contact.node_id, 50).await {
        Ok(messages) => {
            if messages.is_empty() {
                println!(
                    "  {}",
                    console::style("Nenhuma mensagem. Envie a primeira!").dim()
                );
            } else {
                for msg in messages.iter().filter(|m| m.is_displayable()) {
                    println!("  {}", msg.display());
                    // Auto-saves files received from other users (only new ones)
                    if msg.is_file()
                        && msg.from_node_id != state.profile.node_id
                        && let Some(att) = msg.file_attachment()
                    {
                        let key = file_save_key(msg, &att.file_name);
                        if !state.saved_files.contains(&key) {
                            save_received_file_with_progress(&state.profile.username, &att, false);
                            state.saved_files.insert(key);
                            save_saved_files(&state.profile.username, &state.saved_files);
                        }
                    }
                }
                last_count = messages.len();
            }
        }
        Err(e) => {
            println!(
                "  {} Error loading messages: {}",
                console::style("✗").red(),
                e
            );
        }
    }
    println!("{}", console::style("─".repeat(60)).dim());

    // Enables raw mode for non-blocking key reading
    terminal::enable_raw_mode().ok();

    let mut input_buf = String::new();
    let mut prompt_shown = false;
    let mut needs_poll = false;
    let mut last_poll_time = Instant::now();

    loop {
        // 1. Checks sync notifications
        if state.has_new_messages.swap(false, Ordering::Relaxed) {
            let peer_id = state.last_sync_peer.lock().await.take();
            if let Some(ref peer_id) = peer_id {
                state.update_contact_last_seen(peer_id);
            }
            needs_poll = true;
        }

        // Fallback: periodic poll every 2 seconds
        if !needs_poll && last_poll_time.elapsed() >= std::time::Duration::from_secs(2) {
            needs_poll = true;
        }

        // 2. Checks for new messages in the log (when sync or timer occurred)
        if needs_poll {
            needs_poll = false;
            last_poll_time = Instant::now();
            if let Ok(messages) = state.get_dm_messages(&contact.node_id, 50).await
                && messages.len() > last_count
            {
                // Clears the current input line if visible
                if prompt_shown {
                    print!("\r{}\r", " ".repeat(80));
                }
                for msg in messages[last_count..].iter().filter(|m| m.is_displayable()) {
                    print!("  {}\r\n", msg.display());
                    // Auto-saves files received from other users (only new ones)
                    if msg.is_file()
                        && msg.from_node_id != state.profile.node_id
                        && let Some(att) = msg.file_attachment()
                    {
                        let key = file_save_key(msg, &att.file_name);
                        if !state.saved_files.contains(&key) {
                            save_received_file_with_progress(&state.profile.username, &att, true);
                            state.saved_files.insert(key.clone());
                            save_saved_files(&state.profile.username, &state.saved_files);
                        }
                    }
                }
                std::io::stdout().flush().ok();
                last_count = messages.len();
                prompt_shown = false;
            }
        } // end needs_poll

        // 3. Shows prompt if not visible
        if !prompt_shown {
            print!(
                "  {} ",
                console::style(format!("{}:", &state.profile.username))
                    .cyan()
                    .bold()
            );
            if !input_buf.is_empty() {
                print!("{}", &input_buf);
            }
            std::io::stdout().flush().ok();
            prompt_shown = true;
        }

        // 4. Checks keyboard input (non-blocking, 200ms poll)
        if event::poll(std::time::Duration::from_millis(200)).unwrap_or(false)
            && let Ok(Event::Key(key_ev)) = event::read()
        {
            // crossterm 0.27+ fires Press, Release AND Repeat for each key.
            // Only filters Press to avoid duplicate characters.
            if key_ev.kind != KeyEventKind::Press {
                continue;
            }
            match key_ev {
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    print!("\r\n");
                    std::io::stdout().flush().ok();
                    prompt_shown = false;

                    let text = input_buf.trim().to_string();
                    input_buf.clear();

                    match text.as_str() {
                        "/back" | "/sair" | "/exit" => break,
                        "/sync" => {
                            print!("  ⏳ Syncing...\r\n");
                            std::io::stdout().flush().ok();
                            match state.connect_to_contact(&contact.node_id).await {
                                Ok(()) => {
                                    print!("  {}\r\n", console::style("✓ Synced!").green());
                                }
                                Err(e) => {
                                    print!(
                                        "  {}\r\n",
                                        console::style(format!("✗ Error: {}", e)).red()
                                    );
                                }
                            }
                            std::io::stdout().flush().ok();
                            needs_poll = true;
                        }
                        "/open" => {
                            let dir = received_files_dir(&state.profile.username);
                            fs::create_dir_all(&dir).ok();
                            print!("  📂 Opening: {}\r\n", dir.display());
                            std::io::stdout().flush().ok();
                            open_folder(&dir);
                        }
                        "/files" => {
                            let dir = received_files_dir(&state.profile.username);
                            if dir.exists() {
                                print!("  📁 Received files ({}):\r\n", dir.display());
                                if let Ok(entries) = fs::read_dir(&dir) {
                                    for entry in entries.flatten() {
                                        let meta = entry.metadata().ok();
                                        let size = meta
                                            .map(|m| format_file_size(m.len()))
                                            .unwrap_or_default();
                                        print!(
                                            "    - {} ({})\r\n",
                                            entry.file_name().to_string_lossy(),
                                            size
                                        );
                                    }
                                }
                            } else {
                                print!("  No files received yet.\r\n");
                            }
                            std::io::stdout().flush().ok();
                        }
                        "" => { /* just refreshes */ }
                        msg if msg.starts_with("/file ") => {
                            let file_path = msg.strip_prefix("/file ").unwrap().trim();
                            if file_path.is_empty() {
                                print!(
                                    "  {} Usage: /file <file_path>\r\n",
                                    console::style("⚠").yellow()
                                );
                                std::io::stdout().flush().ok();
                            } else {
                                // Step 1/5: Reading file
                                print_progress_bar(0, 5, "Reading file...");
                                match prepare_file_attachment(file_path) {
                                    Ok((attachment, file_name)) => {
                                        let size_str = format_file_size(attachment.file_size);

                                        // Step 2/5: Encoded
                                        print_progress_bar(
                                            1,
                                            5,
                                            &format!("Encoding {} ({})...", file_name, size_str),
                                        );

                                        // Step 3/5: Saving to log
                                        print_progress_bar(2, 5, "Saving to P2P log...");
                                        match state
                                            .store_file_dm(&contact.node_id, &attachment)
                                            .await
                                        {
                                            Ok(()) => {
                                                // Step 4/5: Syncing
                                                print_progress_bar(3, 5, "Syncing with peer...");
                                                let sync_ok = state
                                                    .connect_to_contact(&contact.node_id)
                                                    .await
                                                    .is_ok();

                                                // Step 5/5: Completed
                                                print_progress_bar(
                                                    5,
                                                    5,
                                                    &format!(
                                                        "📤 '{}' sent! ({}{})",
                                                        file_name,
                                                        size_str,
                                                        if sync_ok {
                                                            ", sync ✓"
                                                        } else {
                                                            ", pending sync"
                                                        }
                                                    ),
                                                );
                                                finish_progress_bar();
                                                // Update last_count to prevent poll re-display
                                                if let Ok(messages) = state
                                                    .get_dm_messages(&contact.node_id, 50)
                                                    .await
                                                {
                                                    last_count = messages.len();
                                                }
                                            }
                                            Err(e) => {
                                                finish_progress_bar();
                                                print!(
                                                    "  {} Error saving: {}\r\n",
                                                    console::style("✗").red(),
                                                    e
                                                );
                                                std::io::stdout().flush().ok();
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        finish_progress_bar();
                                        print!("  {} Error: {}\r\n", console::style("✗").red(), e);
                                        std::io::stdout().flush().ok();
                                    }
                                }
                            }
                        }
                        msg => {
                            match state.send_dm(&contact.node_id, msg.to_string()).await {
                                Ok(()) => {
                                    // Replace the prompt line with the formatted message
                                    print!("\x1B[1A\r{}\r", " ".repeat(80));
                                    if let Ok(messages) =
                                        state.get_dm_messages(&contact.node_id, 50).await
                                    {
                                        if let Some(last_msg) = messages.last() {
                                            print!("  {}\r\n", last_msg.display());
                                        }
                                        last_count = messages.len();
                                    }
                                    std::io::stdout().flush().ok();
                                }
                                Err(e) => {
                                    print!(
                                        "  {} Error sending: {}\r\n",
                                        console::style("✗").red(),
                                        e
                                    );
                                    std::io::stdout().flush().ok();
                                }
                            }
                        }
                    }
                }
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => {
                    print!("\r\n");
                    std::io::stdout().flush().ok();
                    break;
                }
                KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                } => {
                    if input_buf.pop().is_some() {
                        print!("\x08 \x08");
                        std::io::stdout().flush().ok();
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {
                    print!("\r\n");
                    std::io::stdout().flush().ok();
                    break;
                }
                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => {
                    input_buf.push(c);
                    print!("{}", c);
                    std::io::stdout().flush().ok();
                }
                _ => {}
            }
        }
    }

    // Disables raw mode on exit
    terminal::disable_raw_mode().ok();
}

// ═══════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "error".to_string()))
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let theme = ColorfulTheme::default();

    // ─── Login / Profile Selection ───
    let username = select_or_create_profile(&theme).unwrap_or_else(|e| {
        eprintln!("\n❌ Error: {}", e);
        eprintln!("💡 Run directly in the terminal:");
        eprintln!("   cargo run --example p2p_chat_demo\n");
        std::process::exit(1);
    });

    // ─── Initialization ───
    println!("\n⏳ Initializing GuardianDB...");
    let mut state = ChatState::new(username).await?;
    state.display_welcome();

    // Checks for pending group invites at startup
    state.check_local_files_for_invites();
    if !state.groups.is_empty() {
        println!(
            "  {}",
            console::style(format!("📫 {} group(s) loaded", state.groups.len())).cyan()
        );
    }

    println!(
        "  {}",
        console::style("💡 Peers on the same local network are discovered via mDNS automatically.")
            .dim()
    );
    println!(
        "  {}",
        console::style("📋 Share your Node ID to add contacts.\n").dim()
    );

    // ─── Background: sync monitoring ───
    //
    // ## GuardianDB feature: EventBus & EventExchangeHeads
    //
    // `db.event_bus()` returns the global event bus — a pub/sub system that
    // emits events for various database activities. Here we subscribe to
    // `EventExchangeHeads`, which fires every time a remote peer exchanges
    // log heads with us during P2P sync.
    //
    // The event contains:
    //   - `event.peer`: the NodeId of the peer who synced
    //   - `event.message.heads`: the log entry hashes exchanged
    //
    // This background task runs for the lifetime of the application and:
    //   1. Sets `has_new_messages` flag so the UI polls for new messages
    //   2. Removes the peer from the "pending sync" set (delivery confirmed)
    //   3. Records the peer ID so the UI can show "synced from <contact>"
    //
    // This enables fully reactive, real-time message delivery without polling
    // the event log constantly — the UI only checks for new data when a sync
    // event actually occurs.
    let event_bus = state.db.event_bus();
    let has_new_messages = state.has_new_messages.clone();
    let last_sync_peer = state.last_sync_peer.clone();
    let pending_sync_bg = state.pending_sync.clone();
    let username_bg = state.profile.username.clone();

    tokio::spawn(async move {
        // Subscribe to EventExchangeHeads — this is the reactive notification
        // channel that tells us when any peer has synced data with us.
        let mut receiver = match event_bus.subscribe::<EventExchangeHeads>().await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to subscribe to sync events: {}", e);
                return;
            }
        };

        loop {
            match receiver.recv().await {
                Ok(event) => {
                    debug!(
                        "P2P sync from peer {} ({} heads)",
                        event.peer,
                        event.message.heads.len()
                    );
                    // Signal the UI that new data is available
                    has_new_messages.store(true, Ordering::Relaxed);
                    let peer_str = event.peer.to_string();
                    {
                        // Mark this peer as successfully synced (no longer pending)
                        let mut pending = pending_sync_bg.lock().await;
                        if pending.remove(&peer_str) {
                            save_pending_sync(&username_bg, &pending);
                        }
                    }
                    *last_sync_peer.lock().await = Some(peer_str);
                }
                Err(e) => {
                    error!("Error receiving sync event: {}", e);
                    sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    // ─── Background: automatic pending message retry ───
    //
    // ## GuardianDB feature: background log rehydration + connect_to_peer retry
    //
    // This background task periodically (every 30 seconds) attempts to deliver
    // unsent messages to offline peers. The pattern is:
    //
    //   1. For each "pending" contact, open/reuse their DM EventLogStore.
    //   2. Re-insert any locally-persisted messages (from JSON) that are missing
    //      from the EventLogStore (using `log.add()`) — this ensures messages
    //      written while the peer was offline are in the replicable log.
    //   3. Attempt `connect_to_peer()` — if the peer is now online, entries
    //      are synced and the contact is removed from the pending set.
    //   4. Repeat the same process for group logs, connecting to each member.
    //
    // This "store-and-forward" pattern leverages EventLogStore's persistence
    // and P2P sync to guarantee eventual delivery even across app restarts.
    {
        let db = state.db.clone();
        let dm_logs_bg = state.dm_logs.clone();
        let profile_node_id = state.profile.node_id.clone();
        let profile_username = state.profile.username.clone();
        let pending_sync = state.pending_sync.clone();

        tokio::spawn(async move {
            sleep(Duration::from_secs(5)).await;
            loop {
                let contacts: Vec<String> = { pending_sync.lock().await.iter().cloned().collect() };

                for contact_node_id in &contacts {
                    let log_name = dm_log_name(&profile_node_id, contact_node_id);

                    // Reuses existing log or creates new one (avoids sled lock conflict)
                    let log = {
                        let mut dm_logs = dm_logs_bg.lock().await;
                        if let Some(existing) = dm_logs.get(&log_name) {
                            existing.clone()
                        } else {
                            let log_options = CreateDBOptions {
                                create: Some(true),
                                store_type: Some("eventlog".to_string()),
                                replicate: Some(true),
                                ..Default::default()
                            };
                            match db.log(&log_name, Some(log_options)).await {
                                Ok(l) => {
                                    if let Err(e) = l.load(usize::MAX).await {
                                        debug!("Warning loading log: {}", e);
                                    }
                                    dm_logs.insert(log_name.clone(), l.clone());
                                    l
                                }
                                Err(e) => {
                                    debug!(
                                        "Retry: error creating log for {}: {}",
                                        &contact_node_id[..contact_node_id.len().min(16)],
                                        e
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    if let Ok(peer_id) = contact_node_id.parse::<NodeId>()
                        && let Some(wrapper) = log.as_any().downcast_ref::<EventLogStoreWrapper>()
                    {
                        match wrapper.connect_to_peer(peer_id).await {
                            Ok(()) => {
                                let mut pending = pending_sync.lock().await;
                                pending.remove(contact_node_id);
                                save_pending_sync(&profile_username, &pending);
                                info!(
                                    "✓ Pending message delivered to {}",
                                    &contact_node_id[..contact_node_id.len().min(16)]
                                );
                            }
                            Err(_) => {
                                debug!(
                                    "Retry: peer {} still offline",
                                    &contact_node_id[..contact_node_id.len().min(16)]
                                );
                            }
                        }
                    }
                }

                // Also syncs group logs
                let groups = load_groups(&profile_username);
                for group in &groups {
                    let grp_log_name = group.log_name();
                    let grp_log = {
                        let mut dm_logs = dm_logs_bg.lock().await;
                        if let Some(existing) = dm_logs.get(&grp_log_name) {
                            existing.clone()
                        } else {
                            let log_options = CreateDBOptions {
                                create: Some(true),
                                store_type: Some("eventlog".to_string()),
                                replicate: Some(true),
                                ..Default::default()
                            };
                            match db.log(&grp_log_name, Some(log_options)).await {
                                Ok(l) => {
                                    if let Err(e) = l.load(usize::MAX).await {
                                        debug!("Warning loading group log: {}", e);
                                    }
                                    dm_logs.insert(grp_log_name.clone(), l.clone());
                                    l
                                }
                                Err(_) => continue,
                            }
                        }
                    };

                    // Connects to each member to sync
                    for member in &group.members {
                        if member.node_id == profile_node_id {
                            continue;
                        }
                        if let Ok(peer_id) = member.node_id.parse::<NodeId>()
                            && let Some(wrapper) =
                                grp_log.as_any().downcast_ref::<EventLogStoreWrapper>()
                        {
                            wrapper.connect_to_peer(peer_id).await.ok();
                        }
                    }
                }

                sleep(Duration::from_secs(30)).await;
            }
        });
    }

    // ─── Main Loop ───
    loop {
        // Checks for group invites in ALL DMs each iteration
        state.check_local_files_for_invites();

        // Checks pending notifications
        if state.has_new_messages.swap(false, Ordering::Relaxed) {
            let peer_id = state.last_sync_peer.lock().await.take();
            if let Some(ref peer_id) = peer_id {
                state.update_contact_last_seen(peer_id);

                // Checks for group invites from the synced peer
                let groups_before = state.groups.len();
                if let Ok(msgs) = state.get_dm_messages(peer_id, 200).await {
                    state.process_group_invites(&msgs);
                }
                let groups_after = state.groups.len();
                if groups_after > groups_before {
                    println!(
                        "\n  {}",
                        console::style(format!(
                            "📫 {} new group(s) received!",
                            groups_after - groups_before
                        ))
                        .cyan()
                        .bold()
                    );
                }

                let contact_name = state
                    .contacts
                    .iter()
                    .find(|c| c.node_id == *peer_id)
                    .map(|c| c.display_name.clone())
                    .unwrap_or_else(|| format!("{}...", &peer_id[..peer_id.len().min(8)]));

                println!(
                    "\n  {}",
                    console::style(format!("🔔 Message synced from {}!", contact_name))
                        .cyan()
                        .bold()
                );
            }
        }

        let menu_items = vec![
            "💬 Chat with contact",
            "👥 Group chat",
            "➕ Add contact",
            "🆕 Create group",
            "➕ Add member to group",
            "📋 My contacts",
            "📋 My groups",
            "👤 My profile",
            "🚪 Exit",
        ];

        let selection = Select::with_theme(&theme)
            .with_prompt("Main Menu")
            .items(&menu_items)
            .default(0)
            .interact()
            .unwrap();

        match selection {
            // Chat with contact (DM)
            0 => {
                if state.contacts.is_empty() {
                    println!(
                        "\n  {}",
                        console::style("No contacts! Add a contact first.").yellow()
                    );
                    continue;
                }

                let contact_items: Vec<String> = state
                    .contacts
                    .iter()
                    .map(|c| {
                        format!(
                            "{} - {}",
                            c.display_name,
                            contact_status_display(c.last_seen)
                        )
                    })
                    .collect();

                let mut items = contact_items;
                items.push("↩ Back".to_string());

                let sel = Select::with_theme(&theme)
                    .with_prompt("Select a contact")
                    .items(&items)
                    .default(0)
                    .interact()
                    .unwrap();

                if sel < state.contacts.len() {
                    chat_mode(&mut state, sel, &theme).await;
                }
            }

            // Group chat
            1 => {
                if state.groups.is_empty() {
                    println!(
                        "\n  {}",
                        console::style("No groups! Create a group first.").yellow()
                    );
                    continue;
                }

                let group_items: Vec<String> = state
                    .groups
                    .iter()
                    .map(|g| format!("{} ({} members)", g.name, g.members.len()))
                    .collect();

                let mut items = group_items;
                items.push("↩ Back".to_string());

                let sel = Select::with_theme(&theme)
                    .with_prompt("Select a group")
                    .items(&items)
                    .default(0)
                    .interact()
                    .unwrap();

                if sel < state.groups.len() {
                    group_chat_mode(&mut state, sel, &theme).await;
                }
            }

            // Add contact
            2 => {
                add_contact_flow(&mut state, &theme).await;
            }

            // Create group
            3 => {
                create_group_flow(&mut state, &theme).await;
            }

            // Add member to group
            4 => {
                add_member_to_group_flow(&mut state, &theme).await;
            }

            // My contacts
            5 => {
                display_contacts(&state);
            }

            // My groups
            6 => {
                display_groups(&state);
            }

            // My profile
            7 => {
                display_profile(&state);
            }

            // Exit
            8 => {
                println!("\n{}", console::style("  Goodbye!").yellow().bold());
                break;
            }

            _ => {}
        }
    }

    Ok(())
}
