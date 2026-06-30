# Read-only replication

GuardianDB supports a **one/two writers, many readers** topology with a **cryptographic**
guarantee: a node designated as a reader can read and replicate the database, but **cannot write
to it** — it cannot insert, delete, or corrupt entries — **even if the machine is compromised**
and the software tampered with.

This document explains the guarantee, how it works, and how to use it. For revoking a writer
that has already been provisioned, see [`NAMESPACE_ROTATION.md`](./NAMESPACE_ROTATION.md).

## The guarantee

For the iroh-docs-based stores (`KeyValueStore` and `DocumentStore`), a reader's inability to
write is **cryptographic**: the reader node never receives the key that authorizes writes. It is
not merely a software check that "every honest node agrees to run".

Motivating use case: a backup app that uses GuardianDB as a shared index of hashes. Several
reader nodes replicate and read the index; only one or two main nodes may modify it. If a reader
is breached, it is acceptable for them to read the data — but not to write it (and thereby make
other nodes delete data or store garbage).

## Why it is safe: the iroh-docs model

The KV/Document stores **are** iroh-docs underneath (`AuthorId`, `Doc`, `NamespaceId`). In
iroh-docs:

- A document (namespace) is identified by a **public key** (`NamespaceId`); the
  **`NamespaceSecret`** is the write capability.
- Every entry is a `SignedEntry` carrying a **namespace signature**, produced with the
  `NamespaceSecret`. Without that secret it is **impossible to forge** an entry that other peers
  will accept — they reject the invalid signature.
- Sharing a doc with `ShareMode::Read` hands out only the `NamespaceId` (the read/verify
  capability), **without** the `NamespaceSecret`.

A node imported in `Read` mode literally lacks the key to sign valid entries. Compromised
software does not change this: it cannot produce what it has no key to sign.

## How it works in GuardianDB

The guarantee is enforced in layers — the cryptographic one is essential; the others are
defense in depth and good ergonomics.

### 1. Per-role capability in the ticket exchange (the cryptographic layer)

KV/Document stores replicate by sharing the **same namespace**. The `DocTicket` exchange happens
over authenticated QUIC: the requester's public key (`EndpointId`) comes from the TLS handshake,
so it **cannot be forged**
([`ticket_exchange.rs`](../src/p2p/network/core/ticket_exchange.rs)).

The node holding the store keeps **two pre-generated tickets** — one read, one write — and hands
out the one matching the requester's **authenticated role**:

- peer in the `write` role (or `write` contains `*`) → receives the **write ticket** (with the
  `NamespaceSecret`);
- peer only in the `read` role (or `read` contains `*`) → receives the **read ticket** (just the
  `NamespaceId`);
- otherwise → denied.

This way the `NamespaceSecret` only leaves the node for authenticated, write-authorized peers.
Readers receive only the public key.

### 2. Enforcement on the reader node (defense in depth)

- **Fail-fast on writes.** A store opened as read-only refuses `put`/`delete` locally, with an
  immediate error, instead of producing entries that peers would discard. The guard sits on the
  public write path (`Store::add_operation`) and on the inherent methods too.
- **No namespace self-creation.** A read-only node with neither a ticket nor a cached namespace
  **fails** (fail-closed) instead of creating an isolated namespace — which would inadvertently
  give it its own write secret.
- **Writability tracked and persisted.** Each store knows whether it holds the secret: `create`
  → writable; import via ticket → according to the `DocTicket`'s capability; reopen → flag
  persisted in the cache. Effective writability is `holds_secret && !read_only`.

## How to use it

The recommended configuration pairs access control on the **writer** with the read-only option
on the **reader**.

### Writer node

Grant the `write` role only to the writer nodes' ids (hex of the `EndpointId`) and leave reads
open. The helper builds this ACL:

```rust
use guardian_db::traits::CreateDBOptions;
use guardian_db::access_control::manifest::CreateAccessControllerOptions;

// hex of each trusted writer's EndpointId: hex::encode(node_id.as_bytes())
let acl = CreateAccessControllerOptions::read_only_replication(vec![writer_hex]);

let kv = db
    .key_value(
        "ro-shared",
        Some(CreateDBOptions {
            access_controller: Some(Box::new(acl)),
            ..Default::default()
        }),
    )
    .await?;

kv.put("k1", b"v1".to_vec()).await?; // replicates automatically
```

The resulting ACL is `write: [writer_hex]`, `read: ["*"]` — that is, only the listed writers
receive a write ticket; any authenticated reader receives a read ticket.

### Reader node

Open the store as read-only. It imports the writer's namespace (read ticket) and never creates
its own:

```rust
use guardian_db::traits::CreateDBOptions;

let kv = db
    .key_value(
        "ro-shared",
        Some(CreateDBOptions {
            read_only: Some(true),
            ..Default::default()
        }),
    )
    .await?;

// Reads and replication work:
let v = kv.get("k1").await?;

// Writes are refused immediately:
assert!(kv.put("x", b"y".to_vec()).await.is_err());
```

Nodes find each other via mDNS (LAN), n0 discovery (internet), or `known_peers`/`connect_to_peer`;
once connected, the reader obtains the read ticket from the writer through the automatic exchange
(gated by the access controller).

## Inherent limitation: revoking a writer requires rotation

The write capability is the **namespace secret**, and it is **symmetric**: all writers share the
same secret. Removing a key from the `write` role in the ACL only blocks **future** write-ticket
grants — it does not retract the secret from a writer that already obtained it.

Truly revoking a compromised writer requires **namespace rotation** (new namespace, new secret,
state migration, ticket redistribution). That procedure, with a tested helper and a runbook, is
in [`NAMESPACE_ROTATION.md`](./NAMESPACE_ROTATION.md).

Revoking **readers**, by contrast, is trivial: simply stop granting them tickets; they keep only
what they already replicated, with no write power.

## Implementation reference

| Component | Location |
|-----------|----------|
| Per-role capability + ticket exchange | [`src/p2p/network/core/ticket_exchange.rs`](../src/p2p/network/core/ticket_exchange.rs) |
| Read/write ticket generation (`ShareMode`) | [`src/p2p/network/core/docs.rs`](../src/p2p/network/core/docs.rs) |
| Ticket provider registration | [`src/p2p/network/core/mod.rs`](../src/p2p/network/core/mod.rs) |
| `read_only` option / writability / guards | [`src/stores/kv_store/mod.rs`](../src/stores/kv_store/mod.rs), [`src/stores/document_store/mod.rs`](../src/stores/document_store/mod.rs) |
| `read_only_replication` ACL helper | [`src/access_control/manifest.rs`](../src/access_control/manifest.rs) |
| Rotation helper | [`src/rotation.rs`](../src/rotation.rs) |

The guarantee is exercised by unit tests (ticket exchange, fail-fast, no-create) and by a
**two real-node** integration test over QUIC
([`tests/integration_readonly.rs`](../tests/integration_readonly.rs)), in which the reader
receives the writer's replicated value but has `put`/`delete` refused, with the writer's state
left intact.
