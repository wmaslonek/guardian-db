# Namespace Rotation (revoking a writer)

## Why this exists

GuardianDB's read-only replication (see [`READ_ONLY_REPLICATION.md`](./READ_ONLY_REPLICATION.md))
gives a **cryptographic** guarantee: a reader node receives only a read `DocTicket` (the
namespace public key) and therefore cannot produce writes that other peers accept.

That guarantee has one inherent limit. For iroh-docs-based stores (`KeyValueStore`,
`DocumentStore`), the **write capability is the namespace secret**, and it is *symmetric*:
every writer shares the same secret. Once a node has received a write ticket, it holds that
secret permanently. Removing its key from the `write` role in the access controller stops
**future** ticket grants, but it **cannot retract a secret an existing writer already holds**.

So if a writer node is compromised, ACL edits are not enough. The only way to truly revoke it
is to **rotate the namespace**: stand up a fresh namespace (new id + new secret), migrate the
state into it, hand new tickets to the parties you still trust, and abandon the old namespace.

## When to rotate

- A writer node (or its key material) is compromised or lost.
- You are removing a writer from the trusted set and need it to lose write power, not just be
  delisted.
- Periodic key hygiene, if your threat model calls for it.

Readers do **not** require rotation to be revoked — simply stop granting them tickets and they
keep only what they already replicated (read-only). Rotation is specifically about **writers**.

## Model: versioned database names

Because the namespace id is cached per store and embedded in every peer's local replica, the
cleanest, least-surprising rotation is to **version the database name**:

```
ro-shared-v1   ->   ro-shared-v2
```

The new name yields a brand-new namespace with a brand-new secret. The compromised writer has
the old secret, which is now worthless because everyone moves to `-v2`.

## Procedure

Run these steps on a **trusted writer** node whose replica is fully synced.

### 1. Open the old store and create the new one

```rust
use guardian_db::traits::CreateDBOptions;
use guardian_db::access_control::manifest::CreateAccessControllerOptions;

// Old namespace (writer view).
let old = db.key_value("ro-shared-v1", None).await?;

// New namespace, with the *remaining* trusted writers only (hex EndpointIds).
let new_acl = CreateAccessControllerOptions::read_only_replication(vec![
    trusted_writer_hex.clone(),
]);
let new = db
    .key_value(
        "ro-shared-v2",
        Some(CreateDBOptions {
            access_controller: Some(Box::new(new_acl)),
            ..Default::default()
        }),
    )
    .await?;
```

### 2. Migrate the state

Use the blessed helper, which copies every key/value pair and fails loudly if the destination
is not writable:

```rust
let copied = guardian_db::rotation::copy_key_value_state(old.as_ref(), new.as_ref()).await?;
tracing::info!("migrated {copied} entries into ro-shared-v2");
```

(For a `DocumentStore`, migrate with the public API: read every document via `query` with an
always-true filter and `put` each into the new store. A typed helper can be added if needed.)

### 3. Redistribute new tickets

The new store auto-registers as a ticket provider gated by its access controller. With the new
ACL:

- **Trusted writers** (listed in the new `write` role) request the store and receive a **write
  ticket** (new secret).
- **Readers** (covered by `read: ["*"]`, or an explicit read list) receive a **read ticket**.

If you distribute tickets out-of-band instead of via automatic exchange, mint them explicitly:

```rust
let write_ticket = new.share_ticket().await?; // write capability
// read tickets are handed out automatically per-requester by the ticket exchange
```

The **compromised** node is simply not in the new `write` role, and never authorized to receive
a write ticket for `-v2`.

### 4. Point readers and writers at the new name

Each remaining node reopens against `ro-shared-v2`:

- Writers: `db.key_value("ro-shared-v2", Some(opts_with_new_acl))`.
- Readers: `db.key_value("ro-shared-v2", Some(CreateDBOptions { read_only: Some(true), ..Default::default() }))`.

### 5. Abandon the old namespace

Stop writing to `ro-shared-v1`. Optionally drop it locally to reclaim space (`Store::drop`).
The old secret held by the compromised node now controls only dead data that no trusted node
replicates.

## Caveats

- **Rotation is disruptive by design.** Every node must reopen against the new name; in-flight
  readers keep the stale `-v1` view until they switch.
- **Migrate from a synced writer.** `copy_key_value_state` copies the writer's *local* view, so
  ensure it has converged before rotating, or you may drop entries that hadn't replicated yet.
- **Last-Write-Wins.** KV/Document stores are LWW CRDTs; if multiple writers were active during
  the incident, reconcile conflicting values before/while migrating.
- **Forward secrecy is not retroactive.** Rotation prevents the compromised node from writing
  *going forward*; data it already exfiltrated from the old namespace was already readable to it.

## Summary

| Action | Effect |
|--------|--------|
| Remove key from `write` role | Stops *future* write-ticket grants only |
| **Rotate namespace** | Compromised writer's secret becomes worthless; trusted set continues on a new namespace |

Read-only revocation is cryptographic and immediate; **write revocation requires rotation** —
that is the inherent cost of a shared namespace secret.
