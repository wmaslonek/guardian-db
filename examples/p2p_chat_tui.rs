//! P2P Chat Demo using GuardianDB - TUI Version with Ratatui
//!
//! Features:
//! - Full TUI interface with Ratatui (menus, chat, forms)
//! - Contact management (add, list, chat)
//! - Username linked to NodeID with persistence
//! - Private conversations (DM) per contact with separate logs
//! - Group chat with multiple members
//! - Automatic message updates via P2P sync
//! - Full persistence (profile, contacts, conversation history)
//! - Contact status (online/offline/recently seen)
//! - File sending and receiving via /file <path>
//!
//! Run multiple chat instances to test P2P communication:
//! ```bash
//! cargo run --example p2p_chat_tui
//! ```

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use guardian_db::{
    guardian::{
        EventLogStoreWrapper, GuardianDB,
        core::{EventExchangeHeads, NewGuardianDBOptions},
        error::{GuardianError, Result},
    },
    p2p::network::{
        client::IrohClient,
        config::{ClientConfig, GossipConfig},
    },
    traits::{BaseGuardianDB, CreateDBOptions, EventLogStore},
};
use iroh::NodeId;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::Mutex,
    time::{Duration, sleep},
};
use tracing::{debug, error, info};
use tracing_subscriber::fmt::MakeWriter;

// ═══════════════════════════════════════════════════════════
// Log Capture (redirect tracing to TUI status bar)
// ═══════════════════════════════════════════════════════════

#[derive(Clone)]
struct LogBuffer {
    last_line: Arc<StdMutex<String>>,
}

impl LogBuffer {
    fn new() -> Self {
        Self {
            last_line: Arc::new(StdMutex::new(String::new())),
        }
    }

    fn get_last(&self) -> String {
        self.last_line.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

struct LogWriter {
    buf: Vec<u8>,
    last_line: Arc<StdMutex<String>>,
}

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        if let Ok(s) = std::str::from_utf8(&self.buf) {
            let trimmed = s.trim();
            if !trimmed.is_empty()
                && let Ok(mut last) = self.last_line.lock()
            {
                *last = trimmed.to_string();
            }
        }
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter {
            buf: Vec::new(),
            last_line: Arc::clone(&self.last_line),
        }
    }
}

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

static TRANSFER_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_transfer_id() -> u64 {
    TRANSFER_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
struct FileTransfer {
    id: u64,
    file_name: String,
    file_size: u64,
    progress: f64,
    phase: String,
    direction: TransferDirection,
    #[allow(dead_code)]
    started_at: Instant,
    completed_at: Option<Instant>,
}

#[derive(Clone, PartialEq)]
enum TransferDirection {
    Upload,
    Download,
}

enum FileOpTarget {
    Dm { contact_idx: usize },
    Group { group_idx: usize },
}

struct PendingFileOp {
    file_path: Option<String>,
    attachment: Option<FileAttachment>,
    file_name: Option<String>,
    transfer_id: u64,
    step: FileOpStep,
    target: FileOpTarget,
}

enum FileOpStep {
    UploadPrepare,
    UploadSend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    from_node_id: String,
    from_username: String,
    content: String,
    timestamp: u64,
    #[serde(default = "default_message_type")]
    message_type: String,
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

    fn new_typing(from_node_id: String, from_username: String, target: String) -> Self {
        Self {
            from_node_id,
            from_username,
            content: String::new(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            message_type: "typing".to_string(),
            target,
        }
    }

    fn is_text(&self) -> bool {
        self.message_type.is_empty() || self.message_type == "text"
    }

    fn is_file(&self) -> bool {
        self.message_type == "file"
    }

    fn is_typing(&self) -> bool {
        self.message_type == "typing"
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

    fn belongs_to_dm(&self) -> bool {
        self.target.is_empty() || self.target.starts_with("dm-")
    }

    fn belongs_to_group(&self, group_log_name: &str) -> bool {
        self.target == group_log_name
    }

    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }

    fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
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

fn messages_dir(username: &str) -> PathBuf {
    profiles_dir().join(username).join("messages")
}

fn messages_path(username: &str, log_name: &str) -> PathBuf {
    messages_dir(username).join(format!("{}.json", log_name))
}

fn load_messages_from_file(username: &str, log_name: &str) -> Vec<ChatMessage> {
    let data = match fs::read_to_string(messages_path(username, log_name)) {
        Ok(d) => d,
        Err(_) => return vec![],
    };
    serde_json::from_str::<Vec<ChatMessage>>(&data).unwrap_or_default()
}

fn save_messages_to_file(username: &str, log_name: &str, messages: &[ChatMessage]) {
    let path = messages_path(username, log_name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    if let Ok(data) = serde_json::to_string_pretty(messages) {
        fs::write(&path, data).ok();
    }
}

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

fn groups_path(username: &str) -> PathBuf {
    profiles_dir().join(username).join("groups.json")
}

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

fn cleanup_mixed_messages(
    username: &str,
    my_node_id: &str,
    contacts: &[Contact],
    groups: &[Group],
) {
    for contact in contacts {
        let log_name = dm_log_name(my_node_id, &contact.node_id);
        let mut msgs = load_messages_from_file(username, &log_name);
        let before = msgs.len();
        msgs.retain(|m| m.belongs_to_dm());
        if msgs.len() < before {
            save_messages_to_file(username, &log_name, &msgs);
        }
    }
    for group in groups {
        let log_name = group.log_name();
        let mut msgs = load_messages_from_file(username, &log_name);
        let before = msgs.len();
        msgs.retain(|m| !m.target.starts_with("dm-"));
        if msgs.len() < before {
            save_messages_to_file(username, &log_name, &msgs);
        }
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

fn save_received_file(username: &str, attachment: &FileAttachment) -> Option<PathBuf> {
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

    let data = BASE64.decode(&attachment.file_data).ok()?;
    fs::write(&path, data).ok()?;
    Some(path)
}

fn received_files_dir(username: &str) -> PathBuf {
    profiles_dir().join(username).join("received_files")
}

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

fn contact_status_info(last_seen: Option<u64>) -> (String, Color) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    match last_seen {
        Some(ts) => {
            let diff = now.saturating_sub(ts);
            if diff < 60 {
                ("● Online".to_string(), Color::Green)
            } else if diff < 300 {
                ("● Recent".to_string(), Color::Yellow)
            } else {
                let mins = diff / 60;
                if mins < 60 {
                    (format!("○ Seen {} min ago", mins), Color::DarkGray)
                } else {
                    let hours = mins / 60;
                    (format!("○ Seen {}h ago", hours), Color::DarkGray)
                }
            }
        }
        None => ("○ Never seen".to_string(), Color::DarkGray),
    }
}

// ═══════════════════════════════════════════════════════════
// Chat State (Backend)
// ═══════════════════════════════════════════════════════════

struct ChatState {
    db: Arc<GuardianDB>,
    profile: UserProfile,
    contacts: Vec<Contact>,
    groups: Vec<Group>,
    #[allow(clippy::type_complexity)]
    dm_logs: Arc<Mutex<HashMap<String, Arc<dyn EventLogStore<Error = GuardianError>>>>>,
    local_messages: HashMap<String, Vec<ChatMessage>>,
    #[allow(dead_code)]
    node_id: NodeId,
    has_new_messages: Arc<AtomicBool>,
    last_sync_peer: Arc<Mutex<Option<String>>>,
    pending_sync: Arc<Mutex<HashSet<String>>>,
    saved_files: HashSet<String>,
    typing_states: Arc<StdMutex<HashMap<String, (String, Instant)>>>,
}

impl ChatState {
    async fn new(username: String) -> Result<Self> {
        let config = ClientConfig {
            enable_pubsub: true,
            enable_discovery_mdns: true,
            enable_discovery_n0: true,
            data_store_path: Some(format!("./chat_data/{}", username).into()),
            gossip: GossipConfig {
                max_message_size: 50 * 1024 * 1024,
                ..Default::default()
            },
            ..Default::default()
        };

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
            typing_states: Arc::new(StdMutex::new(HashMap::new())),
        })
    }

    async fn get_or_create_dm_log(
        &mut self,
        contact_node_id: &str,
    ) -> Result<Arc<dyn EventLogStore<Error = GuardianError>>> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);

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
            debug!("Warning loading log '{}' history: {}", log_name, e);
        }

        let my_node_id = self.profile.node_id.clone();
        let local_msgs = load_messages_from_file(&self.profile.username, &log_name);
        if !local_msgs.is_empty() {
            let existing = log.list(None).await.unwrap_or_default();
            let existing_msgs: Vec<ChatMessage> = existing
                .iter()
                .filter_map(|e| ChatMessage::from_bytes(e.value()))
                .collect();

            for msg in &local_msgs {
                if msg.from_node_id != my_node_id {
                    continue;
                }
                if !msg.belongs_to_dm() {
                    continue;
                }
                let already = existing_msgs.iter().any(|em| {
                    em.timestamp == msg.timestamp
                        && em.from_node_id == msg.from_node_id
                        && em.content == msg.content
                });
                if !already {
                    let _ = log.add(msg.to_bytes()).await;
                }
            }
        }

        dm_logs.insert(log_name, log.clone());
        Ok(log)
    }

    async fn send_dm(&mut self, contact_node_id: &str, content: String) -> Result<()> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);
        let msg = ChatMessage::new(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            content,
            log_name.clone(),
        );
        let log = self.get_or_create_dm_log(contact_node_id).await?;
        log.add(msg.to_bytes()).await?;

        let msgs = self
            .local_messages
            .entry(log_name.clone())
            .or_insert_with(|| load_messages_from_file(&self.profile.username, &log_name));
        msgs.push(msg);
        save_messages_to_file(&self.profile.username, &log_name, msgs);

        {
            let mut pending = self.pending_sync.lock().await;
            pending.insert(contact_node_id.to_string());
            save_pending_sync(&self.profile.username, &pending);
        }
        Ok(())
    }

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
        let msgs = self
            .local_messages
            .entry(log_name.clone())
            .or_insert_with(|| load_messages_from_file(&self.profile.username, &log_name));
        msgs.push(msg);
        save_messages_to_file(&self.profile.username, &log_name, msgs);
        {
            let mut pending = self.pending_sync.lock().await;
            pending.insert(contact_node_id.to_string());
            save_pending_sync(&self.profile.username, &pending);
        }
        Ok(())
    }

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
        let msgs = self
            .local_messages
            .entry(log_name.clone())
            .or_insert_with(|| load_messages_from_file(&self.profile.username, &log_name));
        msgs.push(msg);
        save_messages_to_file(&self.profile.username, &log_name, msgs);
        Ok(())
    }

    async fn get_dm_messages(
        &mut self,
        contact_node_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);
        let username = self.profile.username.clone();

        let log = self.get_or_create_dm_log(contact_node_id).await?;
        let entries = log.list(None).await?;
        let all_parsed: Vec<ChatMessage> = entries
            .iter()
            .filter_map(|entry| ChatMessage::from_bytes(entry.value()))
            .filter(|m| m.belongs_to_dm())
            .collect();

        // Detect typing indicators from recent messages
        {
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if let Ok(mut typing) = self.typing_states.lock() {
                for msg in &all_parsed {
                    if msg.is_typing()
                        && msg.from_node_id != self.profile.node_id
                        && now_secs.saturating_sub(msg.timestamp) < 5
                    {
                        typing.insert(
                            msg.from_node_id.clone(),
                            (msg.from_username.clone(), Instant::now()),
                        );
                    }
                }
            }
        }

        let store_messages: Vec<ChatMessage> =
            all_parsed.into_iter().filter(|m| !m.is_typing()).collect();

        let local = self
            .local_messages
            .entry(log_name.clone())
            .or_insert_with(|| {
                let mut loaded = load_messages_from_file(&username, &log_name);
                loaded.retain(|m| m.belongs_to_dm());
                loaded
            });

        let mut changed = false;
        for sm in &store_messages {
            let already_exists = local.iter().any(|lm| {
                lm.timestamp == sm.timestamp
                    && lm.from_node_id == sm.from_node_id
                    && lm.content == sm.content
            });
            if !already_exists {
                local.push(sm.clone());
                changed = true;
            }
        }
        if changed {
            local.sort_by_key(|m| m.timestamp);
            save_messages_to_file(&username, &log_name, local);
        }

        let start = local.len().saturating_sub(limit);
        Ok(local[start..].to_vec())
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

    async fn connect_to_contact(&mut self, contact_node_id: &str) -> Result<()> {
        let peer_id: NodeId = contact_node_id
            .parse()
            .map_err(|_| GuardianError::Store("Invalid NodeId".to_string()))?;

        let log = self.get_or_create_dm_log(contact_node_id).await?;

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
        let msgs = self
            .local_messages
            .entry(log_name.clone())
            .or_insert_with(|| load_messages_from_file(&self.profile.username, &log_name));
        msgs.push(msg);
        save_messages_to_file(&self.profile.username, &log_name, msgs);
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
            let msgs = load_messages_from_file(&self.profile.username, &log_name);
            self.process_group_invites(&msgs);
        }
    }

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
            debug!("Warning loading group '{}' history: {}", log_name, e);
        }

        let my_node_id = self.profile.node_id.clone();
        let local_msgs = load_messages_from_file(&self.profile.username, &log_name);
        if !local_msgs.is_empty() {
            let existing = log.list(None).await.unwrap_or_default();
            let existing_msgs: Vec<ChatMessage> = existing
                .iter()
                .filter_map(|e| ChatMessage::from_bytes(e.value()))
                .collect();

            for msg in &local_msgs {
                if msg.from_node_id != my_node_id {
                    continue;
                }
                if msg.target.starts_with("dm-") {
                    continue;
                }
                let already = existing_msgs.iter().any(|em| {
                    em.timestamp == msg.timestamp
                        && em.from_node_id == msg.from_node_id
                        && em.content == msg.content
                });
                if !already {
                    let _ = log.add(msg.to_bytes()).await;
                }
            }
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
        let msgs = self
            .local_messages
            .entry(log_name.clone())
            .or_insert_with(|| load_messages_from_file(&self.profile.username, &log_name));
        msgs.push(msg);
        save_messages_to_file(&self.profile.username, &log_name, msgs);
        Ok(())
    }

    async fn send_typing_dm(&mut self, contact_node_id: &str) -> Result<()> {
        let log_name = dm_log_name(&self.profile.node_id, contact_node_id);
        let msg = ChatMessage::new_typing(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            log_name,
        );
        let log = self.get_or_create_dm_log(contact_node_id).await?;
        log.add(msg.to_bytes()).await?;
        Ok(())
    }

    async fn send_typing_group(&mut self, group: &Group) -> Result<()> {
        let msg = ChatMessage::new_typing(
            self.profile.node_id.clone(),
            self.profile.username.clone(),
            group.log_name(),
        );
        let log = self.get_or_create_group_log(group).await?;
        log.add(msg.to_bytes()).await?;
        Ok(())
    }

    async fn get_group_messages(
        &mut self,
        group: &Group,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let log_name = group.log_name();
        let username = self.profile.username.clone();

        let log = self.get_or_create_group_log(group).await?;
        let entries = log.list(None).await?;
        let all_parsed: Vec<ChatMessage> = entries
            .iter()
            .filter_map(|entry| ChatMessage::from_bytes(entry.value()))
            .filter(|m| {
                m.belongs_to_group(&log_name)
                    || (m.target.is_empty() && m.is_text())
                    || m.is_typing()
            })
            .collect();

        // Detect typing indicators from recent messages
        {
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if let Ok(mut typing) = self.typing_states.lock() {
                for msg in &all_parsed {
                    if msg.is_typing()
                        && msg.from_node_id != self.profile.node_id
                        && now_secs.saturating_sub(msg.timestamp) < 5
                    {
                        typing.insert(
                            msg.from_node_id.clone(),
                            (msg.from_username.clone(), Instant::now()),
                        );
                    }
                }
            }
        }

        let store_messages: Vec<ChatMessage> =
            all_parsed.into_iter().filter(|m| !m.is_typing()).collect();

        let local = self
            .local_messages
            .entry(log_name.clone())
            .or_insert_with(|| {
                let mut loaded = load_messages_from_file(&username, &log_name);
                loaded.retain(|m| !m.target.starts_with("dm-"));
                loaded
            });

        let mut changed = false;
        for sm in &store_messages {
            let already_exists = local.iter().any(|lm| {
                lm.timestamp == sm.timestamp
                    && lm.from_node_id == sm.from_node_id
                    && lm.content == sm.content
            });
            if !already_exists {
                local.push(sm.clone());
                changed = true;
            }
        }
        if changed {
            local.sort_by_key(|m| m.timestamp);
            save_messages_to_file(&username, &log_name, local);
        }

        let start = local.len().saturating_sub(limit);
        Ok(local[start..].to_vec())
    }

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
}

// ═══════════════════════════════════════════════════════════
// TUI Types
// ═══════════════════════════════════════════════════════════

#[derive(Clone, PartialEq)]
enum Screen {
    Login,
    NewProfile,
    Loading,
    Main,
    SelectContact,
    SelectGroup,
    DmChat { contact_idx: usize },
    GroupChat { group_idx: usize },
    AddContact { step: u8 },
    CreateGroup { step: u8 },
    SelectGroupForAddMember,
    AddMemberToGroup { group_idx: usize },
    ViewContacts,
    ViewGroups,
    ViewProfile,
}

struct Notification {
    text: String,
    time: Instant,
    is_error: bool,
}

struct App {
    state: Option<ChatState>,
    screen: Screen,
    should_quit: bool,

    // Notifications
    notifications: Vec<Notification>,

    // Login
    login_profiles: Vec<String>,
    login_cursor: usize,

    // Text input (shared)
    input: String,
    cursor_pos: usize,

    // Main menu
    menu_state: ListState,

    // List selection (contacts, groups)
    list_state: ListState,

    // Chat
    chat_messages: Vec<ChatMessage>,
    chat_scroll: usize,
    last_poll: Instant,

    // Add contact
    add_contact_node_id: String,
    add_contact_name: String,

    // Create group
    create_group_name: String,
    create_group_selected: HashSet<usize>,
    create_group_cursor: usize,

    // Add member
    add_member_available: Vec<(usize, String)>,

    // Typing indicator
    last_typing_sent: Instant,

    // File transfers
    file_transfers: Vec<FileTransfer>,
    pending_file_ops: Vec<PendingFileOp>,

    // Log capture
    log_buffer: LogBuffer,
}

const MENU_ITEMS: &[&str] = &[
    " 💬  Chat (DM)",
    " 👥  Group Chat",
    " ➕  Add Contact",
    " 🆕  Create Group",
    " 👤  Add Member",
    " 📋  My Contacts",
    " 📋  My Groups",
    " 👤  My Profile",
    " 🚪  Exit",
];

impl App {
    fn new() -> Self {
        let profiles = find_existing_profiles();
        let mut menu_state = ListState::default();
        menu_state.select(Some(0));
        Self {
            state: None,
            screen: if profiles.is_empty() {
                Screen::NewProfile
            } else {
                Screen::Login
            },
            should_quit: false,
            notifications: Vec::new(),
            login_profiles: profiles,
            login_cursor: 0,
            input: String::new(),
            cursor_pos: 0,
            menu_state,
            list_state: ListState::default(),
            chat_messages: Vec::new(),
            chat_scroll: 0,
            last_poll: Instant::now(),
            add_contact_node_id: String::new(),
            add_contact_name: String::new(),
            create_group_name: String::new(),
            create_group_selected: HashSet::new(),
            create_group_cursor: 0,
            add_member_available: Vec::new(),
            last_typing_sent: Instant::now() - std::time::Duration::from_secs(10),
            file_transfers: Vec::new(),
            pending_file_ops: Vec::new(),
            log_buffer: LogBuffer::new(),
        }
    }

    fn notify(&mut self, text: String) {
        self.notifications.push(Notification {
            text,
            time: Instant::now(),
            is_error: false,
        });
    }

    fn notify_error(&mut self, text: String) {
        self.notifications.push(Notification {
            text,
            time: Instant::now(),
            is_error: true,
        });
    }

    fn clear_old_notifications(&mut self) {
        self.notifications
            .retain(|n| n.time.elapsed() < std::time::Duration::from_secs(8));
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
    }

    fn input_insert(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    fn input_backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .chars()
                .last()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos -= prev;
            self.input.remove(self.cursor_pos);
        }
    }

    fn input_delete(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.input.remove(self.cursor_pos);
        }
    }

    fn input_left(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .chars()
                .last()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos -= prev;
        }
    }

    fn input_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            let next = self.input[self.cursor_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos += next;
        }
    }

    async fn tick(&mut self) {
        self.clear_old_notifications();

        // Remove completed file transfers after 3 seconds
        self.file_transfers.retain(|t| {
            t.completed_at
                .map(|c| c.elapsed() < std::time::Duration::from_secs(3))
                .unwrap_or(true)
        });

        // Process pending file operations (one per tick for progress visibility)
        if !self.pending_file_ops.is_empty() {
            let PendingFileOp {
                file_path,
                attachment,
                file_name,
                transfer_id,
                step,
                target,
            } = self.pending_file_ops.remove(0);
            match step {
                FileOpStep::UploadPrepare => {
                    if let Some(fp) = file_path {
                        if let Some(t) =
                            self.file_transfers.iter_mut().find(|t| t.id == transfer_id)
                        {
                            t.progress = 0.3;
                            t.phase = "Reading file...".into();
                        }
                        match prepare_file_attachment(&fp) {
                            Ok((att, fname)) => {
                                if let Some(t) =
                                    self.file_transfers.iter_mut().find(|t| t.id == transfer_id)
                                {
                                    t.file_name = fname.clone();
                                    t.file_size = att.file_size;
                                    t.progress = 0.6;
                                    t.phase = "Sending...".into();
                                }
                                self.pending_file_ops.insert(
                                    0,
                                    PendingFileOp {
                                        file_path: None,
                                        attachment: Some(att),
                                        file_name: Some(fname),
                                        transfer_id,
                                        step: FileOpStep::UploadSend,
                                        target,
                                    },
                                );
                            }
                            Err(e) => {
                                self.notify_error(format!("Error: {}", e));
                                self.file_transfers.retain(|t| t.id != transfer_id);
                            }
                        }
                    }
                }
                FileOpStep::UploadSend => {
                    if let (Some(att), Some(fname)) = (attachment, file_name) {
                        let size_str = format_file_size(att.file_size);
                        let result = if let Some(state) = self.state.as_mut() {
                            match &target {
                                FileOpTarget::Dm { contact_idx } => {
                                    let cid = state.contacts[*contact_idx].node_id.clone();
                                    state.store_file_dm(&cid, &att).await
                                }
                                FileOpTarget::Group { group_idx } => {
                                    let group = state.groups[*group_idx].clone();
                                    state.store_file_group(&group, &att).await
                                }
                            }
                        } else {
                            Ok(())
                        };
                        match result {
                            Ok(()) => {
                                if let Some(t) =
                                    self.file_transfers.iter_mut().find(|t| t.id == transfer_id)
                                {
                                    t.progress = 1.0;
                                    t.phase = "Done!".into();
                                    t.completed_at = Some(Instant::now());
                                }
                                self.notify(format!("📤 '{}' sent! ({})", fname, size_str));
                            }
                            Err(e) => {
                                self.notify_error(format!("Error sending: {}", e));
                                self.file_transfers.retain(|t| t.id != transfer_id);
                            }
                        }
                        self.last_poll = Instant::now() - std::time::Duration::from_secs(10);
                    }
                }
            }
        }

        // Expire old typing indicators
        if let Some(state) = &self.state
            && let Ok(mut typing) = state.typing_states.lock()
        {
            typing.retain(|_, (_, t)| t.elapsed() < std::time::Duration::from_secs(4));
        }

        // Check for sync events
        let mut sync_notification: Option<String> = None;
        let mut new_groups_count = 0usize;
        if let Some(state) = self.state.as_mut() {
            if state.has_new_messages.swap(false, Ordering::Relaxed) {
                let peer_id = state.last_sync_peer.lock().await.take();
                if let Some(ref peer_id) = peer_id {
                    state.update_contact_last_seen(peer_id);

                    let groups_before = state.groups.len();
                    if let Ok(msgs) = state.get_dm_messages(peer_id, 200).await {
                        state.process_group_invites(&msgs);
                    }
                    new_groups_count = state.groups.len().saturating_sub(groups_before);

                    let contact_name = state
                        .contacts
                        .iter()
                        .find(|c| c.node_id == *peer_id)
                        .map(|c| c.display_name.clone())
                        .unwrap_or_else(|| peer_id[..peer_id.len().min(8)].to_string());

                    sync_notification = Some(format!("🔔 Sync from {}", contact_name));
                }
            }

            // Periodically check all DMs for group invites (catches missed syncs)
            state.check_local_files_for_invites();
        }
        if let Some(notif) = sync_notification {
            self.notify(notif);
        }
        if new_groups_count > 0 {
            self.notify(format!("📫 {} new group(s) received!", new_groups_count));
        }

        // Refresh chat messages periodically
        if self.last_poll.elapsed() >= std::time::Duration::from_secs(2) {
            self.last_poll = Instant::now();
            let mut new_msgs: Option<Vec<ChatMessage>> = None;
            if let Some(state) = self.state.as_mut() {
                match &self.screen {
                    Screen::DmChat { contact_idx } => {
                        let contact_id = state.contacts[*contact_idx].node_id.clone();
                        if let Ok(msgs) = state.get_dm_messages(&contact_id, 100).await
                            && msgs.len() != self.chat_messages.len()
                        {
                            new_msgs = Some(msgs);
                        }
                    }
                    Screen::GroupChat { group_idx } => {
                        let group = state.groups[*group_idx].clone();
                        if let Ok(msgs) = state.get_group_messages(&group, 100).await
                            && msgs.len() != self.chat_messages.len()
                        {
                            new_msgs = Some(msgs);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(msgs) = new_msgs {
                self.auto_save_files(&msgs);
                self.chat_messages = msgs;
                self.chat_scroll = 0;
            }
        }
    }

    fn auto_save_files(&mut self, messages: &[ChatMessage]) {
        let mut new_notifications: Vec<String> = Vec::new();
        let mut new_transfers: Vec<FileTransfer> = Vec::new();
        if let Some(state) = self.state.as_mut() {
            for msg in messages {
                if msg.is_file()
                    && msg.from_node_id != state.profile.node_id
                    && let Some(att) = msg.file_attachment()
                {
                    let key = file_save_key(msg, &att.file_name);
                    if !state.saved_files.contains(&key) {
                        let transfer_id = next_transfer_id();
                        if let Some(path) = save_received_file(&state.profile.username, &att) {
                            new_transfers.push(FileTransfer {
                                id: transfer_id,
                                file_name: att.file_name.clone(),
                                file_size: att.file_size,
                                progress: 1.0,
                                phase: "Done!".into(),
                                direction: TransferDirection::Download,
                                started_at: Instant::now(),
                                completed_at: Some(Instant::now()),
                            });
                            new_notifications.push(format!(
                                "📥 {} salvo em {}",
                                att.file_name,
                                path.display()
                            ));
                        }
                        state.saved_files.insert(key);
                        save_saved_files(&state.profile.username, &state.saved_files);
                    }
                }
            }
        }
        self.file_transfers.extend(new_transfers);
        for msg in new_notifications {
            self.notify(msg);
        }
    }
}

// ═══════════════════════════════════════════════════════════
// TUI Rendering
// ═══════════════════════════════════════════════════════════

fn ui(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Main area
            Constraint::Length(2), // Status bar
        ])
        .split(frame.area());

    match &app.screen {
        Screen::Login => render_login(frame, chunks[0], app),
        Screen::NewProfile => render_new_profile(frame, chunks[0], app),
        Screen::Loading => render_loading(frame, chunks[0]),
        Screen::Main => render_main(frame, chunks[0], app),
        Screen::SelectContact => render_select_contact(frame, chunks[0], app),
        Screen::SelectGroup => render_select_group(frame, chunks[0], app),
        Screen::DmChat { .. } => render_chat(frame, chunks[0], app),
        Screen::GroupChat { .. } => render_chat(frame, chunks[0], app),
        Screen::AddContact { .. } => render_add_contact(frame, chunks[0], app),
        Screen::CreateGroup { .. } => render_create_group(frame, chunks[0], app),
        Screen::SelectGroupForAddMember => render_select_group_for_member(frame, chunks[0], app),
        Screen::AddMemberToGroup { .. } => render_add_member(frame, chunks[0], app),
        Screen::ViewContacts => render_view_contacts(frame, chunks[0], app),
        Screen::ViewGroups => render_view_groups(frame, chunks[0], app),
        Screen::ViewProfile => render_view_profile(frame, chunks[0], app),
    }

    render_status_bar(frame, chunks[1], app);
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    // Line 1: user info + notification
    let mut spans = vec![];

    if let Some(state) = &app.state {
        spans.push(Span::styled(
            format!(" {} ", state.profile.username),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        let id_short = &state.profile.node_id[..state.profile.node_id.len().min(16)];
        spans.push(Span::styled(
            format!("ID: {}...", id_short),
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("C:{} G:{}", state.contacts.len(), state.groups.len()),
            Style::default().fg(Color::Yellow),
        ));
    }

    // Append notification inline
    if let Some(notif) = app.notifications.last() {
        spans.push(Span::raw("  "));
        let style = if notif.is_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        spans.push(Span::styled(format!("│ {}", notif.text), style));
    }

    let status_line = Line::from(spans);
    frame.render_widget(Paragraph::new(status_line), Rect { height: 1, ..area });

    // Line 2: last log message (truncated to fit)
    let log_area = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    let last_log = app.log_buffer.get_last();
    if !last_log.is_empty() {
        let max_len = log_area.width.saturating_sub(2) as usize;
        let display = if last_log.chars().count() > max_len {
            let truncated: String = last_log.chars().take(max_len.saturating_sub(1)).collect();
            format!(" {}…", truncated)
        } else {
            format!(" {}", last_log)
        };
        let line = Line::from(Span::styled(display, Style::default().fg(Color::DarkGray)));
        frame.render_widget(Paragraph::new(line), log_area);
    }
}

fn render_login(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" GuardianDB Chat P2P ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);

    // Title
    let title = Paragraph::new(Line::from(vec![Span::styled(
        "Select a Profile",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]))
    .alignment(Alignment::Center);
    frame.render_widget(title, chunks[0]);

    // Subtitle
    let subtitle = Paragraph::new(Line::from(Span::styled(
        "Use ↑↓ to navigate, Enter to select",
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(subtitle, chunks[1]);

    // Profile list
    let mut items: Vec<ListItem> = app
        .login_profiles
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let style = if i == app.login_cursor {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(format!("  👤 {}  ", name), style)))
        })
        .collect();

    let new_profile_style = if app.login_cursor == app.login_profiles.len() {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };
    items.push(ListItem::new(Line::from(Span::styled(
        "  ➕ Create new profile  ",
        new_profile_style,
    ))));

    let list = List::new(items);
    frame.render_widget(list, chunks[2]);

    // Help
    let help = Paragraph::new(Line::from(Span::styled(
        "[↑↓] Navigate  [Enter] Select  [q] Quit",
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(help, chunks[3]);
}

fn render_new_profile(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Create Profile ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);

    let title = Paragraph::new(Line::from(Span::styled(
        "Choose a username",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(title, chunks[0]);

    // Input field
    let input_block = Block::default()
        .title(" Username ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let input_text = Paragraph::new(Line::from(Span::raw(&app.input))).block(input_block);
    frame.render_widget(input_text, chunks[1]);

    // Position cursor
    frame.set_cursor_position((chunks[1].x + 1 + app.cursor_pos as u16, chunks[1].y + 1));

    let help = Paragraph::new(Line::from(Span::styled(
        "[Enter] Confirm  [Esc] Back",
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(help, chunks[3]);
}

fn render_loading(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" GuardianDB Chat P2P ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = Paragraph::new(vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "⏳ Initializing GuardianDB...",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Connecting to P2P network and loading data",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(text, inner);
}

fn render_main(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(1)])
        .split(area);

    // Sidebar menu
    let menu_block = Block::default()
        .title(" Menu ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let items: Vec<ListItem> = MENU_ITEMS
        .iter()
        .map(|item| ListItem::new(Line::from(Span::raw(*item))))
        .collect();

    let menu = List::new(items)
        .block(menu_block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut menu_state = app.menu_state;
    frame.render_stateful_widget(menu, chunks[0], &mut menu_state);

    // Content area
    let content_block = Block::default()
        .title(" GuardianDB Chat P2P ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    if let Some(state) = &app.state {
        let created = chrono::DateTime::<chrono::Utc>::from(
            UNIX_EPOCH + Duration::from_secs(state.profile.created_at),
        );

        let content = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "Welcome to GuardianDB P2P Chat!",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("  User:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &state.profile.username,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Node ID:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(&state.profile.node_id, Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("  Created:  ", Style::default().fg(Color::DarkGray)),
                Span::raw(created.format("%m/%d/%Y %H:%M").to_string()),
            ]),
            Line::from(vec![
                Span::styled("  Contacts: ", Style::default().fg(Color::DarkGray)),
                Span::raw(state.contacts.len().to_string()),
            ]),
            Line::from(vec![
                Span::styled("  Groups:   ", Style::default().fg(Color::DarkGray)),
                Span::raw(state.groups.len().to_string()),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  💡 Peers on the same local network are discovered via mDNS",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  📋 Share your Node ID to add contacts",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(content_block);

        frame.render_widget(content, chunks[1]);
    } else {
        frame.render_widget(content_block, chunks[1]);
    }
}

fn render_select_contact(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Select a Contact ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let state = match &app.state {
        Some(s) => s,
        None => {
            frame.render_widget(block, area);
            return;
        }
    };

    if state.contacts.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No contacts added!",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Use 'Add Contact' in the menu to get started.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "[Esc] Back",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(block)
        .alignment(Alignment::Center);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = state
        .contacts
        .iter()
        .map(|c| {
            let (status_text, status_color) = contact_status_info(c.last_seen);
            let id_short = &c.node_id[..c.node_id.len().min(12)];
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", c.display_name),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(status_text, Style::default().fg(status_color)),
                Span::styled(
                    format!("  {}...", id_short),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut list_state = app.list_state;
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_select_group(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Select a Group ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let state = match &app.state {
        Some(s) => s,
        None => {
            frame.render_widget(block, area);
            return;
        }
    };

    if state.groups.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No groups created!",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Use 'Create Group' in the menu to get started.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "[Esc] Back",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(block)
        .alignment(Alignment::Center);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = state
        .groups
        .iter()
        .map(|g| {
            let member_names: Vec<&str> =
                g.members.iter().map(|m| m.display_name.as_str()).collect();
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", g.name),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("({} members: {})", g.members.len(), member_names.join(", ")),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut list_state = app.list_state;
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_chat(frame: &mut Frame, area: Rect, app: &App) {
    let state = match &app.state {
        Some(s) => s,
        None => return,
    };

    // Determine chat title
    let (title, subtitle) = match &app.screen {
        Screen::DmChat { contact_idx } => {
            let c = &state.contacts[*contact_idx];
            let (status, color) = contact_status_info(c.last_seen);
            (
                format!(" 💬 Chat with {} ", c.display_name),
                Some((status, color)),
            )
        }
        Screen::GroupChat { group_idx } => {
            let g = &state.groups[*group_idx];
            let member_names: Vec<&str> =
                g.members.iter().map(|m| m.display_name.as_str()).collect();
            (
                format!(" 👥 {} ({} members) ", g.name, g.members.len()),
                Some((member_names.join(", "), Color::DarkGray)),
            )
        }
        _ => ("Chat".to_string(), None),
    };

    let outer_block = Block::default()
        .title(title)
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    // Check for active typing indicators
    let typing_text = {
        let mut text = None;
        if let Ok(typing) = state.typing_states.lock() {
            let now = Instant::now();
            let active_typers: Vec<&str> = match &app.screen {
                Screen::DmChat { contact_idx } => {
                    let contact_id = &state.contacts[*contact_idx].node_id;
                    typing
                        .get(contact_id)
                        .filter(|(_, t)| now.duration_since(*t) < std::time::Duration::from_secs(4))
                        .map(|(name, _)| vec![name.as_str()])
                        .unwrap_or_default()
                }
                Screen::GroupChat { group_idx } => {
                    let group = &state.groups[*group_idx];
                    typing
                        .iter()
                        .filter(|(id, (_, t))| {
                            *id != &state.profile.node_id
                                && group.members.iter().any(|m| &m.node_id == *id)
                                && now.duration_since(*t) < std::time::Duration::from_secs(4)
                        })
                        .map(|(_, (name, _))| name.as_str())
                        .collect()
                }
                _ => vec![],
            };
            if active_typers.len() == 1 {
                text = Some(format!(" ✏ {} is typing...", active_typers[0]));
            } else if active_typers.len() > 1 {
                text = Some(format!(" ✏ {} are typing...", active_typers.join(", ")));
            }
        }
        text
    };

    let transfer_height = app.file_transfers.len().min(3) as u16;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if subtitle.is_some() { 1 } else { 0 }),
            Constraint::Min(1), // Messages
            Constraint::Length(if typing_text.is_some() { 1 } else { 0 }), // Typing
            Constraint::Length(transfer_height), // File transfers
            Constraint::Length(3), // Input
            Constraint::Length(1), // Help
        ])
        .split(inner);

    // Subtitle (status or members)
    if let Some((sub_text, sub_color)) = subtitle {
        let sub = Paragraph::new(Line::from(Span::styled(
            format!(" {}", sub_text),
            Style::default().fg(sub_color),
        )));
        frame.render_widget(sub, chunks[0]);
    }

    // Messages
    let messages_area = chunks[1];
    let visible_height = messages_area.height as usize;

    let displayable: Vec<&ChatMessage> = app
        .chat_messages
        .iter()
        .filter(|m| m.is_displayable())
        .collect();

    let total = displayable.len();
    let end = total.saturating_sub(app.chat_scroll);
    let start = end.saturating_sub(visible_height);

    let lines: Vec<Line> = displayable[start..end]
        .iter()
        .map(|msg| render_message_line(msg, &state.profile.node_id))
        .collect();

    let empty_lines = visible_height.saturating_sub(lines.len());
    let mut all_lines: Vec<Line> = vec![Line::from(""); empty_lines];
    all_lines.extend(lines);

    let messages_widget = Paragraph::new(all_lines).wrap(Wrap { trim: false });
    frame.render_widget(messages_widget, messages_area);

    // Scroll indicator
    if app.chat_scroll > 0 {
        let indicator = Paragraph::new(Line::from(Span::styled(
            format!(" ▲ {} messages above ", app.chat_scroll),
            Style::default().fg(Color::Yellow),
        )))
        .alignment(Alignment::Right);
        frame.render_widget(
            indicator,
            Rect {
                height: 1,
                ..messages_area
            },
        );
    }

    // Typing indicator
    if let Some(ref typing) = typing_text {
        let typing_line = Paragraph::new(Line::from(Span::styled(
            typing.clone(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
        frame.render_widget(typing_line, chunks[2]);
    }

    // File transfer progress bars
    if !app.file_transfers.is_empty() {
        render_file_transfers(frame, chunks[3], &app.file_transfers);
    }

    // Input
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {}> ", state.profile.username));

    let input_widget = Paragraph::new(Line::from(Span::raw(&app.input))).block(input_block);
    frame.render_widget(input_widget, chunks[4]);

    // Position cursor in input
    frame.set_cursor_position((chunks[4].x + 1 + app.cursor_pos as u16, chunks[4].y + 1));

    // Help
    let help = Paragraph::new(Line::from(vec![
        Span::styled("[Enter] ", Style::default().fg(Color::DarkGray)),
        Span::styled("Send", Style::default().fg(Color::Cyan)),
        Span::styled("  [Esc] ", Style::default().fg(Color::DarkGray)),
        Span::styled("Back", Style::default().fg(Color::Cyan)),
        Span::styled(
            "  /sync /file /open /files",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    frame.render_widget(help, chunks[5]);
}

fn render_message_line<'a>(msg: &ChatMessage, my_node_id: &str) -> Line<'a> {
    let datetime =
        chrono::DateTime::<chrono::Utc>::from(UNIX_EPOCH + Duration::from_secs(msg.timestamp));
    let time_str = datetime.format("%H:%M:%S").to_string();

    let is_mine = msg.from_node_id == my_node_id;
    let name_color = if is_mine { Color::Green } else { Color::Cyan };

    if msg.is_file() {
        if let Some(att) = msg.file_attachment() {
            Line::from(vec![
                Span::styled(
                    format!(" [{}] ", time_str),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    msg.from_username.clone(),
                    Style::default().fg(name_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(": 📎 "),
                Span::styled(
                    att.file_name.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" ({})", format_file_size(att.file_size)),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(
                    format!(" [{}] ", time_str),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    msg.from_username.clone(),
                    Style::default().fg(name_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(": [corrupted file]", Style::default().fg(Color::Red)),
            ])
        }
    } else {
        Line::from(vec![
            Span::styled(
                format!(" [{}] ", time_str),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{}: ", msg.from_username),
                Style::default().fg(name_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(msg.content.clone()),
        ])
    }
}

fn render_file_transfers(frame: &mut Frame, area: Rect, transfers: &[FileTransfer]) {
    for (i, transfer) in transfers.iter().enumerate() {
        if i >= area.height as usize {
            break;
        }
        let y = area.y + i as u16;
        let line_area = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };

        let icon = match transfer.direction {
            TransferDirection::Upload => "📤",
            TransferDirection::Download => "📥",
        };

        let bar_width = 20u16.min(area.width.saturating_sub(45));
        let filled = ((transfer.progress * bar_width as f64) as u16).min(bar_width);
        let empty = bar_width.saturating_sub(filled);

        let bar_filled: String = "█".repeat(filled as usize);
        let bar_empty: String = "░".repeat(empty as usize);

        let progress_pct = (transfer.progress * 100.0) as u16;

        let color = if transfer.completed_at.is_some() {
            Color::Green
        } else {
            Color::Yellow
        };

        let size_text = if transfer.file_size > 0 {
            format!(" ({}) ", format_file_size(transfer.file_size))
        } else {
            " ".to_string()
        };

        let line = Line::from(vec![
            Span::raw(format!(" {} ", icon)),
            Span::styled(
                transfer.file_name.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(size_text, Style::default().fg(Color::DarkGray)),
            Span::styled(bar_filled, Style::default().fg(color)),
            Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" {}% ", progress_pct), Style::default().fg(color)),
            Span::styled(transfer.phase.clone(), Style::default().fg(Color::DarkGray)),
        ]);

        frame.render_widget(Paragraph::new(line), line_area);
    }
}

fn render_add_contact(frame: &mut Frame, area: Rect, app: &App) {
    let step = match &app.screen {
        Screen::AddContact { step } => *step,
        _ => 0,
    };

    let block = Block::default()
        .title(" ➕ Add Contact ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let state = match &app.state {
        Some(s) => s,
        None => return,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);

    // Your Node ID
    let your_id = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("  Your Node ID: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&state.profile.node_id, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(Span::styled(
            "  Share this ID with whoever you want to add",
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    frame.render_widget(your_id, chunks[0]);

    // Node ID input
    let node_id_border = if step == 0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let node_id_block = Block::default()
        .title(" Contact's Node ID ")
        .borders(Borders::ALL)
        .border_style(node_id_border);
    let node_id_text = if step == 0 {
        &app.input
    } else {
        &app.add_contact_node_id
    };
    let node_id_widget = Paragraph::new(Line::from(Span::raw(node_id_text))).block(node_id_block);
    frame.render_widget(node_id_widget, chunks[2]);

    // Display name input
    let name_border = if step == 1 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let name_block = Block::default()
        .title(" Contact name ")
        .borders(Borders::ALL)
        .border_style(name_border);
    let name_text = if step == 1 { &app.input } else { "" };
    let name_widget = Paragraph::new(Line::from(Span::raw(name_text))).block(name_block);
    frame.render_widget(name_widget, chunks[4]);

    // Cursor
    if step == 0 {
        frame.set_cursor_position((chunks[2].x + 1 + app.cursor_pos as u16, chunks[2].y + 1));
    } else if step == 1 {
        frame.set_cursor_position((chunks[4].x + 1 + app.cursor_pos as u16, chunks[4].y + 1));
    }

    // Help
    let help_text = if step == 0 {
        "[Enter] Next  [Esc] Cancel"
    } else {
        "[Enter] Add  [Esc] Cancel"
    };
    let help = Paragraph::new(Line::from(Span::styled(
        help_text,
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(help, chunks[6]);
}

fn render_create_group(frame: &mut Frame, area: Rect, app: &App) {
    let step = match &app.screen {
        Screen::CreateGroup { step } => *step,
        _ => 0,
    };

    let block = Block::default()
        .title(" 🆕 Create Group ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let state = match &app.state {
        Some(s) => s,
        None => return,
    };

    if step == 0 {
        // Enter group name
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(inner);

        let title = Paragraph::new(Line::from(Span::styled(
            "Group name:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let input_block = Block::default()
            .title(" Name ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        let input_widget = Paragraph::new(Line::from(Span::raw(&app.input))).block(input_block);
        frame.render_widget(input_widget, chunks[1]);

        frame.set_cursor_position((chunks[1].x + 1 + app.cursor_pos as u16, chunks[1].y + 1));

        let help = Paragraph::new(Line::from(Span::styled(
            "[Enter] Next  [Esc] Cancel",
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(help, chunks[3]);
    } else {
        // Select members
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(inner);

        let title = Paragraph::new(Line::from(vec![
            Span::styled("Group: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &app.create_group_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ({} selected)", app.create_group_selected.len()),
                Style::default().fg(Color::Yellow),
            ),
        ]));
        frame.render_widget(title, chunks[0]);

        let items: Vec<ListItem> = state
            .contacts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let selected = app.create_group_selected.contains(&i);
                let mark = if selected { "✓" } else { " " };
                let style = if i == app.create_group_cursor {
                    Style::default()
                        .fg(Color::Black)
                        .bg(if selected { Color::Green } else { Color::Cyan })
                        .add_modifier(Modifier::BOLD)
                } else if selected {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(Span::styled(
                    format!(" [{}] {} ", mark, c.display_name),
                    style,
                )))
            })
            .collect();

        let list = List::new(items);
        frame.render_widget(list, chunks[1]);

        let help = Paragraph::new(Line::from(Span::styled(
            "[↑↓] Navigate  [Space] Select  [Enter] Create  [Esc] Cancel",
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(help, chunks[2]);
    }
}

fn render_select_group_for_member(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Add Member - Select Group ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let state = match &app.state {
        Some(s) => s,
        None => {
            frame.render_widget(block, area);
            return;
        }
    };

    if state.groups.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No groups! Create a group first.",
                Style::default().fg(Color::Yellow),
            )),
        ])
        .block(block)
        .alignment(Alignment::Center);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = state
        .groups
        .iter()
        .map(|g| {
            ListItem::new(Line::from(Span::styled(
                format!(" {} ({} members)", g.name, g.members.len()),
                Style::default().fg(Color::White),
            )))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut list_state = app.list_state;
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_add_member(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Add Member to Group ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    if app.add_member_available.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "All contacts are already members of this group.",
                Style::default().fg(Color::Yellow),
            )),
        ])
        .block(block)
        .alignment(Alignment::Center);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .add_member_available
        .iter()
        .map(|(_, name)| {
            ListItem::new(Line::from(Span::styled(
                format!(" {} ", name),
                Style::default().fg(Color::White),
            )))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut list_state = app.list_state;
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_view_contacts(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" 📋 My Contacts ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let state = match &app.state {
        Some(s) => s,
        None => {
            frame.render_widget(block, area);
            return;
        }
    };

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.contacts.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No contacts added",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "Use 'Add Contact' in the menu to get started!",
                Style::default().fg(Color::Cyan),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    let mut lines: Vec<Line> = vec![Line::from("")];
    for (i, contact) in state.contacts.iter().enumerate() {
        let (status_text, status_color) = contact_status_info(contact.last_seen);
        let id_preview = &contact.node_id[..contact.node_id.len().min(16)];
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {}. ", i + 1),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                &contact.display_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(status_text, Style::default().fg(status_color)),
        ]));
        lines.push(Line::from(Span::styled(
            format!("     ID: {}...", id_preview),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
    }

    let content = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(content, inner);
}

fn render_view_groups(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" 📋 My Groups ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let state = match &app.state {
        Some(s) => s,
        None => {
            frame.render_widget(block, area);
            return;
        }
    };

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.groups.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No groups created",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "Use 'Create Group' in the menu to get started!",
                Style::default().fg(Color::Cyan),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    let mut lines: Vec<Line> = vec![Line::from("")];
    for (i, group) in state.groups.iter().enumerate() {
        let member_names: Vec<&str> = group
            .members
            .iter()
            .map(|m| m.display_name.as_str())
            .collect();
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {}. ", i + 1),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                &group.name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ({} members)", group.members.len()),
                Style::default().fg(Color::Yellow),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!("     Members: {}", member_names.join(", ")),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
    }

    let content = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(content, inner);
}

fn render_view_profile(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" 👤 My Profile ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let state = match &app.state {
        Some(s) => s,
        None => {
            frame.render_widget(block, area);
            return;
        }
    };

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let created = chrono::DateTime::<chrono::Utc>::from(
        UNIX_EPOCH + Duration::from_secs(state.profile.created_at),
    );

    let content = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  User Profile",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Name:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &state.profile.username,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Node ID:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(&state.profile.node_id, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Created:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(created.format("%m/%d/%Y %H:%M").to_string()),
        ]),
        Line::from(vec![
            Span::styled("  Contacts: ", Style::default().fg(Color::DarkGray)),
            Span::raw(state.contacts.len().to_string()),
        ]),
        Line::from(vec![
            Span::styled("  Groups:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(state.groups.len().to_string()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  📋 Copy the Node ID above to share",
            Style::default().fg(Color::DarkGray),
        )),
    ]);

    frame.render_widget(content, inner);
}

// ═══════════════════════════════════════════════════════════
// Event Handling
// ═══════════════════════════════════════════════════════════

async fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    match &app.screen.clone() {
        Screen::Login => handle_key_login(app, key),
        Screen::NewProfile => handle_key_new_profile(app, key),
        Screen::Loading => {}
        Screen::Main => handle_key_main(app, key),
        Screen::SelectContact => handle_key_select_contact(app, key).await,
        Screen::SelectGroup => handle_key_select_group(app, key).await,
        Screen::DmChat { .. } => handle_key_chat(app, key).await,
        Screen::GroupChat { .. } => handle_key_chat(app, key).await,
        Screen::AddContact { .. } => handle_key_add_contact(app, key).await,
        Screen::CreateGroup { .. } => handle_key_create_group(app, key).await,
        Screen::SelectGroupForAddMember => handle_key_select_group_for_member(app, key),
        Screen::AddMemberToGroup { .. } => handle_key_add_member(app, key).await,
        Screen::ViewContacts | Screen::ViewGroups | Screen::ViewProfile => {
            if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
                app.screen = Screen::Main;
            }
        }
    }
}

fn handle_key_login(app: &mut App, key: KeyEvent) {
    let total = app.login_profiles.len() + 1; // +1 for "create new"
    match key.code {
        KeyCode::Up => {
            if app.login_cursor > 0 {
                app.login_cursor -= 1;
            }
        }
        KeyCode::Down => {
            if app.login_cursor < total - 1 {
                app.login_cursor += 1;
            }
        }
        KeyCode::Enter => {
            if app.login_cursor < app.login_profiles.len() {
                // Selected existing profile
                app.input = app.login_profiles[app.login_cursor].clone();
                app.screen = Screen::Loading;
            } else {
                // Create new
                app.clear_input();
                app.screen = Screen::NewProfile;
            }
        }
        KeyCode::Char('q') => {
            app.should_quit = true;
        }
        _ => {}
    }
}

fn handle_key_new_profile(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let username = app.input.trim().to_string();
            if !username.is_empty() {
                app.screen = Screen::Loading;
            }
        }
        KeyCode::Esc => {
            app.clear_input();
            if app.login_profiles.is_empty() {
                app.should_quit = true;
            } else {
                app.screen = Screen::Login;
            }
        }
        KeyCode::Char(c) => app.input_insert(c),
        KeyCode::Backspace => app.input_backspace(),
        KeyCode::Delete => app.input_delete(),
        KeyCode::Left => app.input_left(),
        KeyCode::Right => app.input_right(),
        _ => {}
    }
}

fn handle_key_main(app: &mut App, key: KeyEvent) {
    let total = MENU_ITEMS.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            let current = app.menu_state.selected().unwrap_or(0);
            if current > 0 {
                app.menu_state.select(Some(current - 1));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let current = app.menu_state.selected().unwrap_or(0);
            if current < total - 1 {
                app.menu_state.select(Some(current + 1));
            }
        }
        KeyCode::Enter => {
            let selected = app.menu_state.selected().unwrap_or(0);
            match selected {
                0 => {
                    // DM chat
                    app.list_state.select(Some(0));
                    app.screen = Screen::SelectContact;
                }
                1 => {
                    // Group chat
                    app.list_state.select(Some(0));
                    app.screen = Screen::SelectGroup;
                }
                2 => {
                    // Add contact
                    app.clear_input();
                    app.add_contact_node_id.clear();
                    app.add_contact_name.clear();
                    app.screen = Screen::AddContact { step: 0 };
                }
                3 => {
                    // Create group
                    let has_contacts = app
                        .state
                        .as_ref()
                        .map(|s| !s.contacts.is_empty())
                        .unwrap_or(false);
                    if !has_contacts {
                        app.notify_error("Add contacts before creating a group!".into());
                    } else {
                        app.clear_input();
                        app.create_group_name.clear();
                        app.create_group_selected.clear();
                        app.create_group_cursor = 0;
                        app.screen = Screen::CreateGroup { step: 0 };
                    }
                }
                4 => {
                    // Add member to group
                    let has_both = app
                        .state
                        .as_ref()
                        .map(|s| !s.groups.is_empty() && !s.contacts.is_empty())
                        .unwrap_or(false);
                    if !has_both {
                        app.notify_error("Need both groups and contacts!".into());
                    } else {
                        app.list_state.select(Some(0));
                        app.screen = Screen::SelectGroupForAddMember;
                    }
                }
                5 => app.screen = Screen::ViewContacts,
                6 => app.screen = Screen::ViewGroups,
                7 => app.screen = Screen::ViewProfile,
                8 => app.should_quit = true,
                _ => {}
            }
        }
        KeyCode::Char('q') => {
            app.should_quit = true;
        }
        _ => {}
    }
}

async fn handle_key_select_contact(app: &mut App, key: KeyEvent) {
    let contact_count = app.state.as_ref().map(|s| s.contacts.len()).unwrap_or(0);

    match key.code {
        KeyCode::Up => {
            let current = app.list_state.selected().unwrap_or(0);
            if current > 0 {
                app.list_state.select(Some(current - 1));
            }
        }
        KeyCode::Down => {
            let current = app.list_state.selected().unwrap_or(0);
            if current < contact_count.saturating_sub(1) {
                app.list_state.select(Some(current + 1));
            }
        }
        KeyCode::Enter => {
            if let Some(idx) = app.list_state.selected()
                && idx < contact_count
            {
                // Enter DM chat
                app.clear_input();
                app.chat_messages.clear();
                app.chat_scroll = 0;
                app.last_poll = Instant::now() - std::time::Duration::from_secs(10);

                // Connect and load messages
                let mut notifs: Vec<String> = Vec::new();
                if let Some(state) = app.state.as_mut() {
                    let contact_id = state.contacts[idx].node_id.clone();
                    match state.connect_to_contact(&contact_id).await {
                        Ok(()) => notifs.push("✓ Connected!".into()),
                        Err(e) => notifs.push(format!("⚠ Offline: {}", e)),
                    }
                    if let Ok(msgs) = state.get_dm_messages(&contact_id, 100).await {
                        app.chat_messages = msgs;
                    }
                }
                for n in notifs {
                    app.notify(n);
                }
                app.auto_save_files(&app.chat_messages.clone());
                app.screen = Screen::DmChat { contact_idx: idx };
            }
        }
        KeyCode::Esc => {
            app.screen = Screen::Main;
        }
        _ => {}
    }
}

async fn handle_key_select_group(app: &mut App, key: KeyEvent) {
    let group_count = app.state.as_ref().map(|s| s.groups.len()).unwrap_or(0);

    match key.code {
        KeyCode::Up => {
            let current = app.list_state.selected().unwrap_or(0);
            if current > 0 {
                app.list_state.select(Some(current - 1));
            }
        }
        KeyCode::Down => {
            let current = app.list_state.selected().unwrap_or(0);
            if current < group_count.saturating_sub(1) {
                app.list_state.select(Some(current + 1));
            }
        }
        KeyCode::Enter => {
            if let Some(idx) = app.list_state.selected()
                && idx < group_count
            {
                app.clear_input();
                app.chat_messages.clear();
                app.chat_scroll = 0;
                app.last_poll = Instant::now() - std::time::Duration::from_secs(10);

                let mut notifs: Vec<String> = Vec::new();
                if let Some(state) = app.state.as_mut() {
                    let group = state.groups[idx].clone();
                    let (ok, fail) = state.sync_group_members(&group).await;
                    if ok > 0 {
                        notifs.push(format!("✓ {} member(s) connected", ok));
                    }
                    if fail > 0 {
                        notifs.push(format!("⚠ {} member(s) offline", fail));
                    }
                    if let Ok(msgs) = state.get_group_messages(&group, 100).await {
                        app.chat_messages = msgs;
                    }
                }
                for n in notifs {
                    app.notify(n);
                }
                app.auto_save_files(&app.chat_messages.clone());
                app.screen = Screen::GroupChat { group_idx: idx };
            }
        }
        KeyCode::Esc => {
            app.screen = Screen::Main;
        }
        _ => {}
    }
}

async fn handle_key_chat(app: &mut App, key: KeyEvent) {
    match key {
        KeyEvent {
            code: KeyCode::Esc, ..
        } => {
            app.screen = Screen::Main;
        }
        KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => {
            app.screen = Screen::Main;
        }
        KeyEvent {
            code: KeyCode::PageUp,
            ..
        } => {
            let displayable = app
                .chat_messages
                .iter()
                .filter(|m| m.is_displayable())
                .count();
            app.chat_scroll = (app.chat_scroll + 5).min(displayable.saturating_sub(1));
        }
        KeyEvent {
            code: KeyCode::PageDown,
            ..
        } => {
            app.chat_scroll = app.chat_scroll.saturating_sub(5);
        }
        KeyEvent {
            code: KeyCode::Enter,
            ..
        } => {
            let text = app.input.trim().to_string();
            app.clear_input();

            if text.is_empty() {
                return;
            }

            match text.as_str() {
                "/back" | "/sair" | "/quit" | "/exit" => {
                    app.screen = Screen::Main;
                }
                "/sync" => {
                    if let Some(state) = app.state.as_mut() {
                        match &app.screen.clone() {
                            Screen::DmChat { contact_idx } => {
                                let contact_id = state.contacts[*contact_idx].node_id.clone();
                                match state.connect_to_contact(&contact_id).await {
                                    Ok(()) => app.notify("✓ Synced!".into()),
                                    Err(e) => app.notify_error(format!("✗ Error: {}", e)),
                                }
                            }
                            Screen::GroupChat { group_idx } => {
                                let group = state.groups[*group_idx].clone();
                                let (ok, fail) = state.sync_group_members(&group).await;
                                app.notify(format!("✓ {} connected, {} offline", ok, fail));
                            }
                            _ => {}
                        }
                    }
                    app.last_poll = Instant::now() - std::time::Duration::from_secs(10);
                }
                "/open" => {
                    if let Some(state) = &app.state {
                        let dir = received_files_dir(&state.profile.username);
                        fs::create_dir_all(&dir).ok();
                        open_folder(&dir);
                        app.notify(format!("📂 Abrindo: {}", dir.display()));
                    }
                }
                "/files" => {
                    if let Some(state) = &app.state {
                        let dir = received_files_dir(&state.profile.username);
                        if dir.exists() {
                            if let Ok(entries) = fs::read_dir(&dir) {
                                let mut file_list = String::from("📁 Files: ");
                                for entry in entries.flatten() {
                                    let name = entry.file_name().to_string_lossy().to_string();
                                    let size = entry
                                        .metadata()
                                        .ok()
                                        .map(|m| format_file_size(m.len()))
                                        .unwrap_or_default();
                                    file_list.push_str(&format!("{} ({}), ", name, size));
                                }
                                app.notify(file_list);
                            }
                        } else {
                            app.notify("No files received yet.".into());
                        }
                    }
                }
                "/members" => {
                    if let Screen::GroupChat { group_idx } = &app.screen
                        && let Some(state) = &app.state
                    {
                        let group = &state.groups[*group_idx];
                        let names: Vec<&str> = group
                            .members
                            .iter()
                            .map(|m| m.display_name.as_str())
                            .collect();
                        app.notify(format!("Members: {}", names.join(", ")));
                    }
                }
                msg if msg.starts_with("/file ") => {
                    let file_path = msg.strip_prefix("/file ").unwrap().trim();
                    if file_path.is_empty() {
                        app.notify_error("Usage: /file <file_path>".into());
                    } else {
                        let target = match &app.screen {
                            Screen::DmChat { contact_idx } => Some(FileOpTarget::Dm {
                                contact_idx: *contact_idx,
                            }),
                            Screen::GroupChat { group_idx } => Some(FileOpTarget::Group {
                                group_idx: *group_idx,
                            }),
                            _ => None,
                        };
                        if let Some(target) = target {
                            let path_display = std::path::Path::new(file_path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| file_path.to_string());
                            let transfer_id = next_transfer_id();
                            app.file_transfers.push(FileTransfer {
                                id: transfer_id,
                                file_name: path_display,
                                file_size: 0,
                                progress: 0.05,
                                phase: "Preparing...".into(),
                                direction: TransferDirection::Upload,
                                started_at: Instant::now(),
                                completed_at: None,
                            });
                            app.pending_file_ops.push(PendingFileOp {
                                file_path: Some(file_path.to_string()),
                                attachment: None,
                                file_name: None,
                                transfer_id,
                                step: FileOpStep::UploadPrepare,
                                target,
                            });
                        }
                    }
                }
                msg => {
                    if let Some(state) = app.state.as_mut() {
                        let result = match &app.screen.clone() {
                            Screen::DmChat { contact_idx } => {
                                let cid = state.contacts[*contact_idx].node_id.clone();
                                state.send_dm(&cid, msg.to_string()).await
                            }
                            Screen::GroupChat { group_idx } => {
                                let group = state.groups[*group_idx].clone();
                                state.send_group_message(&group, msg.to_string()).await
                            }
                            _ => Ok(()),
                        };
                        if let Err(e) = result {
                            app.notify_error(format!("Error sending: {}", e));
                        }
                    }
                    // Force refresh
                    app.last_poll = Instant::now() - std::time::Duration::from_secs(10);
                    app.chat_scroll = 0;
                }
            }
        }
        KeyEvent {
            code: KeyCode::Char(c),
            ..
        } => {
            app.input_insert(c);
            // Send typing indicator (throttled to every 2 seconds)
            if app.last_typing_sent.elapsed() >= std::time::Duration::from_secs(2) {
                app.last_typing_sent = Instant::now();
                let screen = app.screen.clone();
                if let Some(state) = app.state.as_mut() {
                    match &screen {
                        Screen::DmChat { contact_idx } => {
                            let cid = state.contacts[*contact_idx].node_id.clone();
                            let _ = state.send_typing_dm(&cid).await;
                        }
                        Screen::GroupChat { group_idx } => {
                            let group = state.groups[*group_idx].clone();
                            let _ = state.send_typing_group(&group).await;
                        }
                        _ => {}
                    }
                }
            }
        }
        KeyEvent {
            code: KeyCode::Backspace,
            ..
        } => {
            app.input_backspace();
        }
        KeyEvent {
            code: KeyCode::Delete,
            ..
        } => {
            app.input_delete();
        }
        KeyEvent {
            code: KeyCode::Left,
            ..
        } => {
            app.input_left();
        }
        KeyEvent {
            code: KeyCode::Right,
            ..
        } => {
            app.input_right();
        }
        KeyEvent {
            code: KeyCode::Home,
            ..
        } => {
            app.cursor_pos = 0;
        }
        KeyEvent {
            code: KeyCode::End, ..
        } => {
            app.cursor_pos = app.input.len();
        }
        _ => {}
    }
}

async fn handle_key_add_contact(app: &mut App, key: KeyEvent) {
    let step = match &app.screen {
        Screen::AddContact { step } => *step,
        _ => return,
    };

    match key.code {
        KeyCode::Enter => {
            if step == 0 {
                // Validate Node ID
                let node_id = app.input.trim().to_string();
                if node_id.is_empty() {
                    app.screen = Screen::Main;
                    return;
                }
                if node_id.parse::<NodeId>().is_err() {
                    app.notify_error("Invalid Node ID!".into());
                    return;
                }
                if let Some(state) = &app.state {
                    if node_id == state.profile.node_id {
                        app.notify_error("You cannot add yourself!".into());
                        return;
                    }
                    if state.contacts.iter().any(|c| c.node_id == node_id) {
                        app.notify_error("Contact already exists!".into());
                        app.screen = Screen::Main;
                        return;
                    }
                }
                app.add_contact_node_id = node_id;
                app.clear_input();
                app.screen = Screen::AddContact { step: 1 };
            } else {
                // Add the contact
                let name = app.input.trim().to_string();
                if name.is_empty() {
                    app.notify_error("Name cannot be empty!".into());
                    return;
                }
                let node_id = app.add_contact_node_id.clone();
                let mut notifs: Vec<(String, bool)> = Vec::new();
                if let Some(state) = app.state.as_mut()
                    && state.add_contact(node_id.clone(), name.clone())
                {
                    notifs.push((format!("✓ Contact '{}' added!", name), false));
                    match state.connect_to_contact(&node_id).await {
                        Ok(()) => {
                            notifs.push(("✓ Connected and syncing!".into(), false));
                        }
                        Err(e) => {
                            notifs.push((
                                format!("⚠ Offline: {}. Will retry when chatting.", e),
                                false,
                            ));
                        }
                    }
                }
                for (msg, is_err) in notifs {
                    if is_err {
                        app.notify_error(msg);
                    } else {
                        app.notify(msg);
                    }
                }
                app.clear_input();
                app.screen = Screen::Main;
            }
        }
        KeyCode::Esc => {
            app.clear_input();
            app.screen = Screen::Main;
        }
        KeyCode::Char(c) => app.input_insert(c),
        KeyCode::Backspace => app.input_backspace(),
        KeyCode::Delete => app.input_delete(),
        KeyCode::Left => app.input_left(),
        KeyCode::Right => app.input_right(),
        _ => {}
    }
}

async fn handle_key_create_group(app: &mut App, key: KeyEvent) {
    let step = match &app.screen {
        Screen::CreateGroup { step } => *step,
        _ => return,
    };

    if step == 0 {
        // Entering group name
        match key.code {
            KeyCode::Enter => {
                let name = app.input.trim().to_string();
                if !name.is_empty() {
                    app.create_group_name = name;
                    app.clear_input();
                    app.create_group_selected.clear();
                    app.create_group_cursor = 0;
                    app.screen = Screen::CreateGroup { step: 1 };
                }
            }
            KeyCode::Esc => {
                app.clear_input();
                app.screen = Screen::Main;
            }
            KeyCode::Char(c) => app.input_insert(c),
            KeyCode::Backspace => app.input_backspace(),
            KeyCode::Delete => app.input_delete(),
            KeyCode::Left => app.input_left(),
            KeyCode::Right => app.input_right(),
            _ => {}
        }
    } else {
        // Selecting members
        let contact_count = app.state.as_ref().map(|s| s.contacts.len()).unwrap_or(0);

        match key.code {
            KeyCode::Up => {
                if app.create_group_cursor > 0 {
                    app.create_group_cursor -= 1;
                }
            }
            KeyCode::Down => {
                if app.create_group_cursor < contact_count.saturating_sub(1) {
                    app.create_group_cursor += 1;
                }
            }
            KeyCode::Char(' ') => {
                let idx = app.create_group_cursor;
                if app.create_group_selected.contains(&idx) {
                    app.create_group_selected.remove(&idx);
                } else {
                    app.create_group_selected.insert(idx);
                }
            }
            KeyCode::Enter => {
                if app.create_group_selected.is_empty() {
                    app.notify_error("Select at least one member!".into());
                    return;
                }
                let name = app.create_group_name.clone();
                let members: Vec<usize> = app.create_group_selected.iter().copied().collect();

                let mut notif_msg = String::new();
                if let Some(state) = app.state.as_mut() {
                    let group = state.create_group(name.clone(), &members);
                    notif_msg = format!(
                        "✓ Group '{}' created with {} members!",
                        name,
                        group.members.len()
                    );

                    // Send invites and sync
                    for member in &group.members {
                        if member.node_id != state.profile.node_id {
                            state.send_group_invite(&member.node_id, &group).await.ok();
                            state.connect_to_contact(&member.node_id).await.ok();
                        }
                    }
                    state.sync_group_members(&group).await;
                }
                if !notif_msg.is_empty() {
                    app.notify(notif_msg);
                }
                app.screen = Screen::Main;
            }
            KeyCode::Esc => {
                app.screen = Screen::Main;
            }
            _ => {}
        }
    }
}

fn handle_key_select_group_for_member(app: &mut App, key: KeyEvent) {
    let group_count = app.state.as_ref().map(|s| s.groups.len()).unwrap_or(0);

    match key.code {
        KeyCode::Up => {
            let current = app.list_state.selected().unwrap_or(0);
            if current > 0 {
                app.list_state.select(Some(current - 1));
            }
        }
        KeyCode::Down => {
            let current = app.list_state.selected().unwrap_or(0);
            if current < group_count.saturating_sub(1) {
                app.list_state.select(Some(current + 1));
            }
        }
        KeyCode::Enter => {
            if let Some(idx) = app.list_state.selected()
                && idx < group_count
            {
                // Build available contacts (not already in group)
                if let Some(state) = &app.state {
                    let existing: HashSet<String> = state.groups[idx]
                        .members
                        .iter()
                        .map(|m| m.node_id.clone())
                        .collect();
                    app.add_member_available = state
                        .contacts
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| !existing.contains(&c.node_id))
                        .map(|(i, c)| (i, c.display_name.clone()))
                        .collect();
                }
                if app.add_member_available.is_empty() {
                    app.notify("All contacts are already members!".into());
                } else {
                    app.list_state.select(Some(0));
                    app.screen = Screen::AddMemberToGroup { group_idx: idx };
                }
            }
        }
        KeyCode::Esc => {
            app.screen = Screen::Main;
        }
        _ => {}
    }
}

async fn handle_key_add_member(app: &mut App, key: KeyEvent) {
    let group_idx = match &app.screen {
        Screen::AddMemberToGroup { group_idx } => *group_idx,
        _ => return,
    };

    match key.code {
        KeyCode::Up => {
            let current = app.list_state.selected().unwrap_or(0);
            if current > 0 {
                app.list_state.select(Some(current - 1));
            }
        }
        KeyCode::Down => {
            let current = app.list_state.selected().unwrap_or(0);
            if current < app.add_member_available.len().saturating_sub(1) {
                app.list_state.select(Some(current + 1));
            }
        }
        KeyCode::Enter => {
            if let Some(sel) = app.list_state.selected()
                && sel < app.add_member_available.len()
            {
                let (contact_idx, ref name) = app.add_member_available[sel];
                let name = name.clone();
                let mut notif: Option<(String, bool)> = None;
                if let Some(state) = app.state.as_mut() {
                    if state.add_member_to_group(group_idx, contact_idx) {
                        notif = Some((format!("✓ '{}' added to group!", name), false));

                        let group = state.groups[group_idx].clone();
                        for member in &group.members {
                            if member.node_id != state.profile.node_id {
                                state.send_group_invite(&member.node_id, &group).await.ok();
                                state.connect_to_contact(&member.node_id).await.ok();
                            }
                        }
                        state.sync_group_members(&group).await;
                    } else {
                        notif = Some(("Member already exists in group.".into(), true));
                    }
                }
                if let Some((msg, is_err)) = notif {
                    if is_err {
                        app.notify_error(msg);
                    } else {
                        app.notify(msg);
                    }
                }
                app.screen = Screen::Main;
            }
        }
        KeyCode::Esc => {
            app.screen = Screen::Main;
        }
        _ => {}
    }
}

// ═══════════════════════════════════════════════════════════
// Background Tasks
// ═══════════════════════════════════════════════════════════

fn spawn_background_tasks(state: &ChatState) {
    // Sync event monitoring
    let event_bus = state.db.event_bus();
    let has_new_messages = state.has_new_messages.clone();
    let last_sync_peer = state.last_sync_peer.clone();
    let pending_sync_bg = state.pending_sync.clone();
    let username_bg = state.profile.username.clone();

    tokio::spawn(async move {
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
                    has_new_messages.store(true, Ordering::Relaxed);
                    let peer_str = event.peer.to_string();
                    {
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

    // Background retry for pending messages
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

                let local_msgs = load_messages_from_file(&profile_username, &log_name);
                let existing = log.list(None).await.unwrap_or_default();
                let existing_msgs: Vec<ChatMessage> = existing
                    .iter()
                    .filter_map(|e| ChatMessage::from_bytes(e.value()))
                    .collect();

                for msg in &local_msgs {
                    if msg.from_node_id != profile_node_id {
                        continue;
                    }
                    if !msg.belongs_to_dm() {
                        continue;
                    }
                    let already = existing_msgs.iter().any(|em| {
                        em.timestamp == msg.timestamp
                            && em.from_node_id == msg.from_node_id
                            && em.content == msg.content
                    });
                    if !already {
                        let _ = log.add(msg.to_bytes()).await;
                    }
                }

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

            // Also sync group logs
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

                let local_msgs = load_messages_from_file(&profile_username, &grp_log_name);
                let existing = grp_log.list(None).await.unwrap_or_default();
                let existing_msgs: Vec<ChatMessage> = existing
                    .iter()
                    .filter_map(|e| ChatMessage::from_bytes(e.value()))
                    .collect();
                for msg in &local_msgs {
                    if msg.from_node_id != profile_node_id {
                        continue;
                    }
                    if msg.target.starts_with("dm-") {
                        continue;
                    }
                    let already = existing_msgs.iter().any(|em| {
                        em.timestamp == msg.timestamp
                            && em.from_node_id == msg.from_node_id
                            && em.content == msg.content
                    });
                    if !already {
                        let _ = grp_log.add(msg.to_bytes()).await;
                    }
                }

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

// ═══════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let log_buffer = LogBuffer::new();

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "warn,guardian_db=info,iroh=warn".to_string()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_writer(log_buffer.clone())
        .with_ansi(false)
        .compact()
        .init();

    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, log_buffer).await;
    ratatui::restore();

    if let Err(ref e) = result {
        eprintln!("Error: {}", e);
    }

    result
}

async fn run_app(
    terminal: &mut ratatui::DefaultTerminal,
    log_buffer: LogBuffer,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    app.log_buffer = log_buffer;

    loop {
        terminal.draw(|f| ui(f, &app))?;

        // Handle Loading screen transition
        if app.screen == Screen::Loading {
            let username = app.input.trim().to_string();
            terminal.draw(|f| ui(f, &app))?;

            match ChatState::new(username).await {
                Ok(mut state) => {
                    cleanup_mixed_messages(
                        &state.profile.username,
                        &state.profile.node_id,
                        &state.contacts,
                        &state.groups,
                    );
                    state.check_local_files_for_invites();
                    spawn_background_tasks(&state);
                    app.state = Some(state);
                    app.screen = Screen::Main;
                }
                Err(e) => {
                    app.notify_error(format!("Initialization failed: {}", e));
                    app.screen = Screen::Login;
                    app.login_profiles = find_existing_profiles();
                    if app.login_profiles.is_empty() {
                        app.screen = Screen::NewProfile;
                    }
                }
            }
            continue;
        }

        // Poll for events
        if event::poll(std::time::Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            handle_key(&mut app, key).await;
        }

        // Periodic tick
        app.tick().await;

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
