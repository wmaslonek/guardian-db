# GuardianDB - P2P Chat Demo

> **🐛 Known Bugs — Help us fix them!**
>
>
> | # | Severity | Description | Affected Version |
> |---|----------|-------------|------------------|
> | 1 | 🟡 Medium | **Progress bar not synchronized** — During file transfers, the progress bar does not appear simultaneously in the sender's and recipient's windows. Visual feedback only occurs on one side. | Both |
> | 2 | 🟡 Medium | **Broken progress bar rendering** — In the Dialoguer version, the file transfer progress bar renders across 4 separate lines instead of updating on a single line, cluttering the terminal interface. | Dialoguer |
> | 3 | 🔴 **Critical** | **Performance degradation after file transfer** — After sending a file, typing in the chat becomes progressively slower. Sending multiple files exponentially worsens the issue, rendering the chat unusable. Likely a memory leak or accumulation of unreleased handlers. | Both |
> | 4 | 🟡 Medium | **Messages grouped by sender with wrong order and timestamp** — In some connections, messages are grouped by user instead of appearing in correct chronological order. Timestamps are also displayed incorrectly, suggesting a sorting or clock synchronization issue during log merging. | Both |
> | 5 | 🔴 **Critical** | **Chat loses sync and stops responding after file transfer** — In some connections, after a file is sent, the chat silently loses synchronization: messages sent by User A are not received by User B (and vice versa). The issue persists until the chat window is restarted, after which messages reappear and sync resumes. | Both |
>
> **How to Contribute:** If you have experience with Rust, async/await, or TUIs, your help is welcome! Open an issue or submit a PR in the repository. The bugs are primarily located in the `store_file_dm`, `store_file_group` functions and in the progress rendering loop.

---

## Overview

This is a decentralized peer-to-peer chat example built with GuardianDB and Iroh. The project offers two interface versions, both sharing the same backend functionality:

| Version | File | Interface | Best for |
|---------|------|-----------|----------|
| **Dialoguer** | `p2p_chat_demo.rs` | Interactive terminal menus | Simplicity, basic terminals |
| **Ratatui (TUI)** | `p2p_chat_tui.rs` | Graphical terminal interface | Full visual experience |

### Shared Features

**Direct P2P communication** using the Iroh network (QUIC).<br>
**Direct messages (DM)** with private per-contact conversations.<br>
**Group chat** with multiple members.<br>
**Automatic synchronization** of messages between peers.<br>
**File sending and receiving** via `/file <path>` (up to 5 MB).<br>
**Contact management** (add, list, online/offline status).<br>
**Group invitations** sent automatically via DM.<br>
**Decentralized storage** with EventLogStore.<br>
**Peer discovery** via mDNS (local network) and n0.computer (internet).<br>
**Full persistence** (profile, contacts, groups, messages, received files).<br>
**Automatic retry** of pending messages in background.

---

## How to Run

### Prerequisites

Make sure Rust is installed:

```bash
rustc --version
cargo --version
```

### Dialoguer Version (Classic Terminal)

```bash
cargo run --example p2p_chat_demo
```

Menu-based interactive interface with [dialoguer](https://docs.rs/dialoguer). Navigate with arrow keys and press Enter to select.

### Ratatui Version (Graphical TUI)

```bash
cargo run --example p2p_chat_tui
```

Full graphical terminal interface with [ratatui](https://docs.rs/ratatui). Takes up the entire screen with panels, visual menus, and a status bar.

> **Note:** Both versions share the same profile data. You can switch between them without losing messages or contacts.

---

## Dialoguer Version — Usage Guide

### Getting Started

1. Run `cargo run --example p2p_chat_demo`
2. On first run, enter a username
3. On subsequent runs, select an existing profile or create a new one

### Main Menu

| Option | Description |
|--------|-------------|
| 💬 Chat with contact | Opens a DM conversation with a contact |
| 👥 Group chat | Opens a group chat |
| ➕ Add contact | Adds a contact by Node ID |
| 🆕 Create group | Creates a group with selected contacts |
| ➕ Add member to group | Adds a member to an existing group |
| 📋 My contacts | Lists contacts with status |
| 📋 My groups | Lists groups and members |
| 👤 My profile | Shows profile and Node ID |
| 🚪 Exit | Closes the chat |

### Chat Commands (DM and Group)

Inside a conversation, type special commands:

| Command | Action |
|---------|--------|
| `/back` or `Esc` | Returns to the main menu |
| `/sync` | Forces synchronization with the peer/group |
| `/file <path>` | Sends a file (max 5 MB) |
| `/files` | Lists received files |
| `/open` | Opens the received files folder |
| `/members` | Lists group members (group chat only) |
| `/exit` or `/sair` | Returns to the main menu |

### Flow: Adding a Contact

1. In the menu, select **"➕ Add contact"**
2. Your **Node ID** will be displayed — share it with the other person
3. Paste the contact's **Node ID**
4. Enter a display name for the contact
5. The connection will be attempted automatically

### Flow: Creating a Group

1. Select **"🆕 Create group"**
2. Enter the group name
3. Select members one by one (toggle with Enter)
4. Choose **"✅ Finish"** to create
5. Invitations are sent automatically via DM to each member

---

## Ratatui (TUI) Version — Usage Guide

### Getting Started

1. Run `cargo run --example p2p_chat_tui`
2. The login screen displays existing profiles — use ↑/↓ and Enter to select
3. To create a new profile, select the corresponding option and type the name

### Screens and Navigation

The TUI has multiple navigable screens:

| Screen | How to access | Keys |
|--------|--------------|------|
| **Login** | Initial | ↑↓ Enter, `q` quit |
| **Main Menu** | After login | ↑↓ `j`/`k` Enter, `q` quit |
| **Contact Selection** | Menu → Chat DM | ↑↓ Enter, Esc back |
| **Group Selection** | Menu → Group Chat | ↑↓ Enter, Esc back |
| **Chat (DM/Group)** | After selecting contact/group | Type text, Enter send, Esc back |
| **Add Contact** | Menu → Add Contact | Tab switches field, Enter confirms, Esc cancels |
| **Create Group** | Menu → Create Group | Tab/Enter selects members, Esc cancels |
| **View Contacts** | Menu → My Contacts | Esc or `q` back |
| **View Groups** | Menu → My Groups | Esc or `q` back |
| **View Profile** | Menu → My Profile | Esc or `q` back |

### Global TUI Shortcuts

| Key | Action |
|-----|--------|
| `Esc` | Returns to previous screen |
| `q` | Quit (in menu/login) |
| `Ctrl+C` | Returns to menu (in chat) |
| `PageUp` / `PageDown` | Scroll messages in chat |
| `Home` / `End` | Beginning/end of text field |
| `←` `→` | Navigate within typed text |
| `Delete` | Removes character to the right |

### Chat Commands (TUI)

The same commands from the Dialoguer version work in the TUI:

| Command | Action |
|---------|--------|
| `/back` or `/quit` | Returns to menu |
| `/sync` | Synchronizes with peers |
| `/file <path>` | Sends file with visual progress bar |
| `/files` | Notification with list of received files |
| `/open` | Opens downloads folder in file manager |
| `/members` | Notification with group members |

### TUI-Exclusive Features

**Status bar** — Displays notifications, application logs, and sync alerts in real time.<br>
**Typing indicator** — Shows when the other peer is typing (throttled every 2s).<br>
**Visual progress bar** — For file sending/receiving, integrated into the interface.<br>
**Redirected logs** — Tracing logs are captured and displayed in the status bar (without cluttering the screen).<br>
**Temporary notifications** — Success/error messages appear for 8 seconds in the status bar.<br>
**Vim-key navigation** — `j`/`k` to navigate the main menu.

---

## Version Comparison

| Feature | Dialoguer | Ratatui TUI |
|---------|-----------|-------------|
| Interface | Terminal selection menus | Full-screen graphical panels |
| DM Chat | ✅ Real-time with raw mode | ✅ Real-time with rendering |
| Group Chat | ✅ | ✅ |
| File Transfer | ✅ Text progress bar | ✅ Visual progress bar |
| Typing Indicator | ❌ | ✅ |
| Message Scrolling | Last 50 displayed | ✅ PageUp/PageDown |
| System Logs | stderr | Captured in status bar |
| Resizing | N/A | ✅ Adapts automatically |
| Vim-style Navigation | ❌ | ✅ (`j`/`k`) |

---

## Connecting Peers

### Step by Step

1. **Alice** starts the chat and copies the **Node ID** displayed in her profile
2. **Bob** starts the chat and selects **"Add contact"**
3. **Bob** pastes Alice's Node ID and gives it a name (e.g., "Alice")
4. The connection is attempted automatically
5. **Alice** repeats the process by adding Bob as a contact

> **Tip:** Peers on the same local network are discovered automatically via mDNS. For peers on different networks, the connection uses n0.computer relay servers.

### Bidirectional Connection

For full communication, **both peers must add each other as contacts**. This ensures that both have the DM log configured for synchronization.

---

## Architecture

### Main Components

```
┌─────────────────────────────────────────┐
│         Chat Application (UI)           │
│  ┌─────────────────┐ ┌───────────────┐  │
│  │ p2p_chat_demo.rs│ │p2p_chat_tui.rs│  │
│  │  (Dialoguer)    │ │  (Ratatui)    │  │
│  └────────┬────────┘ └──────┬────────┘  │
│           └──────┬──────────┘           │
├──────────────────┼──────────────────────┤
│           ChatState (Backend)           │
│  ┌──────────────┐  ┌────────────────┐   │
│  │   Profiles   │  │ Local Messages │   │
│  │  Contacts    │  │  (JSON files)  │   │
│  │   Groups     │  │                │   │
│  └──────────────┘  └────────────────┘   │
├─────────────────────────────────────────┤
│               GuardianDB                │
│  ┌──────────────┐  ┌────────────────┐   │
│  │ EventLogStore│  │   EventBus     │   │
│  │  (Messages)  │  │(Notifications) │   │
│  └──────────────┘  └────────────────┘   │
├─────────────────────────────────────────┤
│           Iroh Network Layer            │
│  ┌──────────────┐  ┌────────────────┐   │
│  │   Endpoint   │  │  DirectChannel │   │
│  │  (P2P QUIC)  │  │  (Messages)    │   │
│  └──────────────┘  └────────────────┘   │
└─────────────────────────────────────────┘
```

### Message Flow

1. **User sends a message:**
   - Serialized as JSON (`ChatMessage`)
   - Added to the local EventLogStore
   - Persisted in the local JSON file (`messages/<log_name>.json`)
   - EventLogStore emits an update event

2. **Automatic synchronization:**
   - EventBus notifies connected peers via `EventExchangeHeads`
   - Heads (pointers to the most recent entries) are exchanged
   - Peers synchronize missing messages
   - Background task retries sync every 30 seconds

3. **Message reception:**
   - Peer receives notification of a new head
   - Message is deserialized and merged with local history
   - User interface is updated (polled every 2 seconds)

### Deterministic Logs

Each conversation uses a deterministic log name:<br>
**DM:** `dm-<short_id_A>-<short_id_B>` (IDs sorted alphabetically so both peers generate the same name)<br>
**Group:** `grp-<group_hash>` (based on name, creator, and timestamp)

---

## Data Structure

### Data Persistence

All data is stored locally:

```
./chat_profiles/<username>/
├── profile.json          # User profile (username, node_id, created_at)
├── contacts.json         # Contact list
├── groups.json           # Group list
├── pending_sync.json     # Contacts with pending sync messages
├── saved_files.json      # Tracking of already saved files
├── messages/
│   ├── dm-<id>-<id>.json   # History for each DM
│   └── grp-<hash>.json     # History for each group
└── received_files/
    └── *.* (received files)

./chat_data/<username>/   # Iroh backend data (QUIC, keys)
./chat_db/<username>/     # GuardianDB database (sled)
```

To start with clean data, delete these folders:
```bash
rm -rf ./chat_profiles ./chat_data ./chat_db
```

### Message Format

Each message is serialized as JSON:

```json
{
  "from_node_id": "sender_node_id",
  "from_username": "Alice",
  "content": "Hello, world!",
  "timestamp": 1739836800,
  "message_type": "text",
  "target": "dm-abc123-def456"
}
```

Supported message types:<br>
`text` — Regular text message<br>
`file` — File (content contains JSON with `file_name`, `file_size`, `file_data` in base64)<br>
`group_invite` — Group invitation (content contains group JSON)<br>
`typing` — Typing indicator (TUI version only)

---

## Security Features

**Cryptographic identity** — Each peer has a NodeId based on an Ed25519 key.<br>
**TLS connections** — All P2P communication uses QUIC over TLS.<br>
**Integrity verification** — Messages are signed and verified.<br>
**NAT traversal** — Automatic hole punching via relay servers.<br>
**No central server** — Messages are exchanged directly between peers.<br>
**Local data** — All history is stored only on the user's machine.

---

## Advanced Configuration

### Environment Variables

```bash
# Dialoguer version with debug logs
RUST_LOG=debug cargo run --example p2p_chat_demo

# TUI version with debug logs (redirected to status bar)
RUST_LOG=debug cargo run --example p2p_chat_tui

# GuardianDB-only logs
RUST_LOG=guardian_db=trace cargo run --example p2p_chat_demo

# Detailed Iroh network logs
RUST_LOG=iroh=debug cargo run --example p2p_chat_demo
```

### Network Configuration

In the source files, you can adjust:

```rust
let config = ClientConfig {
    enable_pubsub: true,
    enable_discovery_mdns: true,  // Local network discovery
    enable_discovery_n0: true,    // Internet discovery
    port: 0,                      // Random port (or set a specific one)
    data_store_path: Some(format!("./chat_data/{}", username).into()),
    gossip: GossipConfig {
        max_message_size: 50 * 1024 * 1024,  // 50MB to support file transfers
        ..Default::default()
    },
    ..Default::default()
};
```

### File Size Limit

The default file transfer limit is **5 MB**. To change it, modify the constant in the code:

```rust
const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024; // 5 MB
```

---

## Testing

### Basic Test with 2 Peers

1. Open 2 terminals
2. Run the chat in each one with different usernames
3. Add each other as contacts using the Node ID
4. Send messages and verify synchronization

### Test with 3+ Peers (Mesh)

1. Run 3 chat instances in different terminals
2. Connect Peer A → Peer B
3. Connect Peer B → Peer C
4. Send a message from Peer C
5. Verify it appears in A, B, and C (mesh network)

### Persistence Test

1. Run the chat and send messages
2. Close the chat (Exit / `q`)
3. Run the chat again with the same username
4. Verify that messages were preserved

### Offline Synchronization Test

1. Run 2 disconnected instances
2. Send messages in both
3. Connect the peers (by adding each other)
4. Verify that both synchronize the complete history

### File Transfer Test

1. Connect two peers
2. In the chat, type `/file /path/to/file.txt`
3. Verify that the file appears automatically in the other peer's `received_files/` folder
4. Use `/files` to list and `/open` to open the folder

### Group Test

1. Create a group with 2+ members
2. Verify that invitations arrive via DM
3. Send messages in the group
4. Verify synchronization across all members

### Cross-Version Test

1. Run the Dialoguer version in one terminal
2. Run the Ratatui TUI version in another terminal
3. Use the same profile or different profiles
4. Verify that messages synchronize between the two versions

---

## Troubleshooting

### "Failed to connect to peer"

- Verify that the Node ID is correct and complete
- Make sure both peers are on the same network (or with active relay)
- Wait a few seconds for discovery
- The system automatically retries the connection every 30 seconds

### "Messages not appearing"

- Check logs with `RUST_LOG=debug`
- Confirm that the P2P connection was established
- Explicitly connect the peers (both directions)
- Use `/sync` to force manual synchronization

### "Typing indicator not appearing"

- This feature is exclusive to the **Ratatui TUI** version
- The indicator is throttled (sent at most every 2 seconds)
- Disappears after 3 seconds of inactivity

### "File too large"

- The default limit is 5 MB per file
- Larger files are rejected with an error message
- To change the limit, modify `MAX_FILE_SIZE` in the code

### "Compilation error"

```bash
# Clean and recompile
cargo clean
cargo build --example p2p_chat_demo
cargo build --example p2p_chat_tui
```

### "TUI with distorted visuals"

- Make sure to use a Unicode-compatible terminal (Windows Terminal, iTerm2, Alacritty)
- Resize the terminal if the panels appear cut off
- The minimum recommended terminal size is 80x24 characters

---

## Additional Resources

[GuardianDB Documentation](../README.md)<br>
[Iroh Documentation](https://docs.rs/iroh)<br>
[Iroh Gossip](https://docs.rs/iroh-gossip)<br>
[Ratatui Documentation](https://docs.rs/ratatui)<br>
[Dialoguer Documentation](https://docs.rs/dialoguer)

**Have fun exploring GuardianDB!**

## License

This example is under the same license as GuardianDB: MIT/Apache-2.0
