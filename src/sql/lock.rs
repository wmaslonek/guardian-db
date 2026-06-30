//! A PostgreSQL-style lock manager for the single-node gateway.
//!
//! The default gateway is a single coordinator, so it can offer real lock
//! semantics: the eight table-level lock modes with PostgreSQL's exact conflict
//! matrix, the four row-level lock modes, advisory locks (session/transaction,
//! shared/exclusive), `NOWAIT` / `SKIP LOCKED`, blocking waits with
//! `lock_timeout`, and deadlock detection (victim aborted with SQLSTATE 40P01).
//! Lock state is exposed through `pg_catalog.pg_locks`.
//!
//! Locks are held by a *session* (one statement runs at a time per connection)
//! and tagged with a [`LockScope`] (released at transaction end, or at session
//! end for session-level advisory locks).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

use crate::relational::RelError;

/// Identifies a connection (lock holder). One statement runs at a time per
/// session, so session granularity is sufficient for the wait-for graph.
pub type SessionId = u64;

/// The object a lock is taken on.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum LockObject {
    /// A whole table, by OID.
    Table(u32),
    /// A single row: `(table OID, row id)`.
    Row(u32, String),
    /// An advisory lock keyed by a 64-bit value.
    Advisory(i64),
}

impl LockObject {
    fn kind(&self) -> &'static str {
        match self {
            LockObject::Table(_) => "relation",
            LockObject::Row(_, _) => "tuple",
            LockObject::Advisory(_) => "advisory",
        }
    }

    fn describe(&self) -> String {
        match self {
            LockObject::Table(oid) => format!("relation {oid}"),
            LockObject::Row(oid, rid) => format!("row ({oid},{rid})"),
            LockObject::Advisory(key) => format!("advisory {key}"),
        }
    }
}

/// Lock modes across the three object kinds. On any given object every recorded
/// mode is of the matching kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum LockMode {
    // Table-level (ordered weakest to strongest).
    AccessShare,
    RowShare,
    RowExclusive,
    ShareUpdateExclusive,
    Share,
    ShareRowExclusive,
    Exclusive,
    AccessExclusive,
    // Row-level.
    ForKeyShare,
    ForShare,
    ForNoKeyUpdate,
    ForUpdate,
    // Advisory.
    AdvisoryShared,
    AdvisoryExclusive,
}

impl LockMode {
    fn table_index(self) -> Option<usize> {
        Some(match self {
            LockMode::AccessShare => 0,
            LockMode::RowShare => 1,
            LockMode::RowExclusive => 2,
            LockMode::ShareUpdateExclusive => 3,
            LockMode::Share => 4,
            LockMode::ShareRowExclusive => 5,
            LockMode::Exclusive => 6,
            LockMode::AccessExclusive => 7,
            _ => return None,
        })
    }

    fn row_index(self) -> Option<usize> {
        Some(match self {
            LockMode::ForKeyShare => 0,
            LockMode::ForShare => 1,
            LockMode::ForNoKeyUpdate => 2,
            LockMode::ForUpdate => 3,
            _ => return None,
        })
    }

    /// PostgreSQL's canonical name for this mode (used by `pg_locks`).
    pub fn pg_name(self) -> &'static str {
        match self {
            LockMode::AccessShare => "AccessShareLock",
            LockMode::RowShare => "RowShareLock",
            LockMode::RowExclusive => "RowExclusiveLock",
            LockMode::ShareUpdateExclusive => "ShareUpdateExclusiveLock",
            LockMode::Share => "ShareLock",
            LockMode::ShareRowExclusive => "ShareRowExclusiveLock",
            LockMode::Exclusive => "ExclusiveLock",
            LockMode::AccessExclusive => "AccessExclusiveLock",
            LockMode::ForKeyShare => "KeyShareLock",
            LockMode::ForShare => "ShareLock",
            LockMode::ForNoKeyUpdate => "NoKeyUpdateLock",
            LockMode::ForUpdate => "ExclusiveLock",
            LockMode::AdvisoryShared | LockMode::AdvisoryExclusive => "AdvisoryLock",
        }
    }

    /// Does a request for `self` conflict with an already-held `other` (on the
    /// same object)?
    pub fn conflicts(self, other: LockMode) -> bool {
        // Table conflict sets (bit b set => index b conflicts), symmetric.
        const TABLE: [u8; 8] = [
            1 << 7,
            (1 << 6) | (1 << 7),
            (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7),
            (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7),
            (1 << 2) | (1 << 3) | (1 << 5) | (1 << 6) | (1 << 7),
            (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7),
            (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7),
            0xFF,
        ];
        const ROW: [u8; 4] = [
            1 << 3,
            (1 << 2) | (1 << 3),
            (1 << 1) | (1 << 2) | (1 << 3),
            (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3),
        ];
        if let (Some(a), Some(b)) = (self.table_index(), other.table_index()) {
            return (TABLE[a] >> b) & 1 == 1;
        }
        if let (Some(a), Some(b)) = (self.row_index(), other.row_index()) {
            return (ROW[a] >> b) & 1 == 1;
        }
        // Advisory: shared/shared coexist; any exclusive conflicts.
        matches!(self, LockMode::AdvisoryExclusive) || matches!(other, LockMode::AdvisoryExclusive)
    }
}

/// When is a lock released?
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockScope {
    /// Released at transaction end (table, row, advisory-xact locks).
    Transaction,
    /// Held until the session releases it or disconnects (advisory session locks).
    Session,
}

/// How to behave when a lock cannot be granted immediately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitPolicy {
    /// Block until the lock is available (the default).
    Wait,
    /// Fail immediately with `LockNotAvailable` (55P03).
    NoWait,
    /// Do not wait; report that the object is locked so the caller can skip it.
    SkipLocked,
}

/// Outcome of an acquire attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Acquired {
    Granted,
    /// `SKIP LOCKED` and the object was locked by another session.
    Skipped,
}

#[derive(Clone, Debug)]
struct Holding {
    holder: SessionId,
    mode: LockMode,
    scope: LockScope,
}

#[derive(Default)]
struct Inner {
    granted: HashMap<LockObject, Vec<Holding>>,
    /// What each currently-blocked session is waiting for (for deadlock cycles).
    waiting: HashMap<SessionId, (LockObject, LockMode)>,
}

impl Inner {
    /// Can `holder` be granted `mode` on `object` right now? (Own locks never
    /// conflict.)
    fn grantable(&self, object: &LockObject, mode: LockMode, holder: SessionId) -> bool {
        match self.granted.get(object) {
            None => true,
            Some(holders) => holders
                .iter()
                .all(|h| h.holder == holder || !mode.conflicts(h.mode)),
        }
    }

    fn grant(&mut self, object: LockObject, mode: LockMode, holder: SessionId, scope: LockScope) {
        self.granted
            .entry(object)
            .or_default()
            .push(Holding { holder, mode, scope });
    }

    /// Sessions that `who` is currently waiting behind (holders of conflicting
    /// locks on the object it wants).
    fn blockers(&self, who: SessionId) -> Vec<SessionId> {
        let Some((object, mode)) = self.waiting.get(&who) else {
            return Vec::new();
        };
        let Some(holders) = self.granted.get(object) else {
            return Vec::new();
        };
        let mut out: Vec<SessionId> = holders
            .iter()
            .filter(|h| h.holder != who && mode.conflicts(h.mode))
            .map(|h| h.holder)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Is `start` part of a wait-for cycle (i.e. reachable from itself)?
    fn deadlocked(&self, start: SessionId) -> bool {
        let mut visited = HashSet::new();
        let mut stack: Vec<SessionId> = self.blockers(start);
        while let Some(node) = stack.pop() {
            if node == start {
                return true;
            }
            if visited.insert(node) {
                stack.extend(self.blockers(node));
            }
        }
        false
    }

    fn release_where(&mut self, mut pred: impl FnMut(&Holding) -> bool) {
        self.granted.retain(|_, holders| {
            holders.retain(|h| !pred(h));
            !holders.is_empty()
        });
    }
}

/// A row in `pg_locks`.
#[derive(Clone, Debug)]
pub struct LockRecord {
    pub locktype: String,
    pub object: String,
    pub mode: String,
    pub holder: SessionId,
    pub granted: bool,
}

/// The lock manager. Shared across all sessions via `Arc`.
pub struct LockManager {
    inner: Mutex<Inner>,
    notify: Notify,
    next_session: AtomicU64,
}

impl Default for LockManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LockManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            notify: Notify::new(),
            next_session: AtomicU64::new(1),
        }
    }

    /// Allocate a fresh session id.
    pub fn new_session(&self) -> SessionId {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// Non-blocking acquire. Returns `true` if granted, `false` if a conflicting
    /// lock is held by another session.
    pub fn try_acquire(
        &self,
        holder: SessionId,
        object: LockObject,
        mode: LockMode,
        scope: LockScope,
    ) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.grantable(&object, mode, holder) {
            inner.grant(object, mode, holder, scope);
            true
        } else {
            false
        }
    }

    /// Is `object` locked in a way that conflicts with `mode` by some *other*
    /// session right now? (Used for `SKIP LOCKED` scans.)
    pub fn is_conflicting(&self, holder: SessionId, object: &LockObject, mode: LockMode) -> bool {
        let inner = self.inner.lock().unwrap();
        !inner.grantable(object, mode, holder)
    }

    /// Acquire a lock, blocking per `wait`, with deadlock detection and an
    /// optional `timeout`.
    pub async fn acquire(
        &self,
        holder: SessionId,
        object: LockObject,
        mode: LockMode,
        scope: LockScope,
        wait: WaitPolicy,
        timeout: Option<Duration>,
    ) -> Result<Acquired, RelError> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            // Register interest *before* checking, so a release between the
            // check and the await is not lost.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            {
                let mut inner = self.inner.lock().unwrap();
                if inner.grantable(&object, mode, holder) {
                    inner.grant(object.clone(), mode, holder, scope);
                    inner.waiting.remove(&holder);
                    return Ok(Acquired::Granted);
                }
                match wait {
                    WaitPolicy::NoWait => {
                        inner.waiting.remove(&holder);
                        return Err(RelError::LockNotAvailable(object.describe()));
                    }
                    WaitPolicy::SkipLocked => {
                        inner.waiting.remove(&holder);
                        return Ok(Acquired::Skipped);
                    }
                    WaitPolicy::Wait => {
                        inner.waiting.insert(holder, (object.clone(), mode));
                        if inner.deadlocked(holder) {
                            inner.waiting.remove(&holder);
                            return Err(RelError::DeadlockDetected {
                                detail: format!(
                                    "process {holder} waits for {}; a cycle was found",
                                    object.describe()
                                ),
                            });
                        }
                    }
                }
            }

            // Wait for a release notification (or the lock timeout).
            match deadline {
                None => notified.await,
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        self.inner.lock().unwrap().waiting.remove(&holder);
                        return Err(RelError::LockNotAvailable(format!(
                            "lock timeout on {}",
                            object.describe()
                        )));
                    }
                    let _ = tokio::time::timeout(d - now, notified).await;
                }
            }
        }
    }

    /// Release one held lock matching `(holder, object, mode)`; returns whether
    /// one was found (used by `pg_advisory_unlock`).
    pub fn release_one(&self, holder: SessionId, object: &LockObject, mode: LockMode) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let mut removed = false;
        if let Some(holders) = inner.granted.get_mut(object) {
            if let Some(pos) = holders
                .iter()
                .position(|h| h.holder == holder && h.mode == mode)
            {
                holders.remove(pos);
                removed = true;
            }
            if holders.is_empty() {
                inner.granted.remove(object);
            }
        }
        drop(inner);
        if removed {
            self.notify.notify_waiters();
        }
        removed
    }

    /// Release all of a session's transaction-scoped locks (at COMMIT/ROLLBACK).
    pub fn release_transaction(&self, holder: SessionId) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.release_where(|h| h.holder == holder && h.scope == LockScope::Transaction);
            inner.waiting.remove(&holder);
        }
        self.notify.notify_waiters();
    }

    /// Release a session's session-scoped (advisory) locks (`pg_advisory_unlock_all`).
    pub fn release_session_advisory(&self, holder: SessionId) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.release_where(|h| h.holder == holder && h.scope == LockScope::Session);
        }
        self.notify.notify_waiters();
    }

    /// Release every lock held by a session (at disconnect).
    pub fn release_session(&self, holder: SessionId) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.release_where(|h| h.holder == holder);
            inner.waiting.remove(&holder);
        }
        self.notify.notify_waiters();
    }

    /// Snapshot of all granted and waiting locks (for `pg_locks`).
    pub fn snapshot(&self) -> Vec<LockRecord> {
        let inner = self.inner.lock().unwrap();
        let mut out = Vec::new();
        for (object, holders) in &inner.granted {
            for h in holders {
                out.push(LockRecord {
                    locktype: object.kind().to_string(),
                    object: object.describe(),
                    mode: h.mode.pg_name().to_string(),
                    holder: h.holder,
                    granted: true,
                });
            }
        }
        for (holder, (object, mode)) in &inner.waiting {
            out.push(LockRecord {
                locktype: object.kind().to_string(),
                object: object.describe(),
                mode: mode.pg_name().to_string(),
                holder: *holder,
                granted: false,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn table_conflict_matrix_matches_postgres() {
        use LockMode::*;
        // A few well-known cells.
        assert!(!AccessShare.conflicts(RowExclusive)); // readers and writers coexist
        assert!(AccessShare.conflicts(AccessExclusive)); // DDL blocks readers
        assert!(!Share.conflicts(Share)); // two SHARE coexist
        assert!(Share.conflicts(RowExclusive)); // SHARE blocks writes
        assert!(RowExclusive.conflicts(Share));
        assert!(ShareUpdateExclusive.conflicts(ShareUpdateExclusive)); // self-conflict
        assert!(AccessExclusive.conflicts(AccessShare));
        assert!(!RowShare.conflicts(RowExclusive));
        // Symmetry over all pairs.
        let all = [
            AccessShare, RowShare, RowExclusive, ShareUpdateExclusive, Share,
            ShareRowExclusive, Exclusive, AccessExclusive,
        ];
        for a in all {
            for b in all {
                assert_eq!(a.conflicts(b), b.conflicts(a), "asymmetry {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn row_conflict_matrix() {
        use LockMode::*;
        assert!(!ForKeyShare.conflicts(ForKeyShare));
        assert!(!ForKeyShare.conflicts(ForShare));
        assert!(ForKeyShare.conflicts(ForUpdate));
        assert!(ForShare.conflicts(ForNoKeyUpdate));
        assert!(ForUpdate.conflicts(ForUpdate));
        let all = [ForKeyShare, ForShare, ForNoKeyUpdate, ForUpdate];
        for a in all {
            for b in all {
                assert_eq!(a.conflicts(b), b.conflicts(a));
            }
        }
    }

    #[test]
    fn advisory_conflict() {
        use LockMode::*;
        assert!(!AdvisoryShared.conflicts(AdvisoryShared));
        assert!(AdvisoryShared.conflicts(AdvisoryExclusive));
        assert!(AdvisoryExclusive.conflicts(AdvisoryExclusive));
    }

    #[tokio::test]
    async fn blocks_then_grants_after_release() {
        let lm = Arc::new(LockManager::new());
        let s1 = lm.new_session();
        let s2 = lm.new_session();
        let obj = LockObject::Row(1, "r".into());
        lm.acquire(s1, obj.clone(), LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::Wait, None)
            .await
            .unwrap();

        let lm2 = lm.clone();
        let obj2 = obj.clone();
        let handle = tokio::spawn(async move {
            lm2.acquire(s2, obj2, LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::Wait, None)
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!handle.is_finished(), "s2 should be blocked behind s1");

        lm.release_transaction(s1);
        let got = tokio::time::timeout(Duration::from_secs(2), handle).await.unwrap().unwrap();
        assert_eq!(got.unwrap(), Acquired::Granted);
    }

    #[tokio::test]
    async fn nowait_fails_immediately() {
        let lm = Arc::new(LockManager::new());
        let s1 = lm.new_session();
        let s2 = lm.new_session();
        let obj = LockObject::Table(5);
        lm.acquire(s1, obj.clone(), LockMode::AccessExclusive, LockScope::Transaction, WaitPolicy::Wait, None)
            .await
            .unwrap();
        let err = lm
            .acquire(s2, obj, LockMode::AccessShare, LockScope::Transaction, WaitPolicy::NoWait, None)
            .await
            .unwrap_err();
        assert_eq!(err.sqlstate(), "55P03");
    }

    #[tokio::test]
    async fn skip_locked_reports_skip() {
        let lm = Arc::new(LockManager::new());
        let s1 = lm.new_session();
        let s2 = lm.new_session();
        let obj = LockObject::Row(1, "r".into());
        lm.acquire(s1, obj.clone(), LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::Wait, None)
            .await
            .unwrap();
        let got = lm
            .acquire(s2, obj, LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::SkipLocked, None)
            .await
            .unwrap();
        assert_eq!(got, Acquired::Skipped);
    }

    #[tokio::test]
    async fn deadlock_is_detected() {
        let lm = Arc::new(LockManager::new());
        let s1 = lm.new_session();
        let s2 = lm.new_session();
        let a = LockObject::Row(1, "a".into());
        let b = LockObject::Row(1, "b".into());
        lm.acquire(s1, a.clone(), LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::Wait, None).await.unwrap();
        lm.acquire(s2, b.clone(), LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::Wait, None).await.unwrap();

        let lm1 = lm.clone();
        let (a1, b1) = (a.clone(), b.clone());
        let h1 = tokio::spawn(async move {
            lm1.acquire(s1, b1, LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::Wait, None).await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        // s2 now requests a -> closes the cycle and must detect a deadlock.
        let res2 = lm
            .acquire(s2, a, LockMode::ForUpdate, LockScope::Transaction, WaitPolicy::Wait, None)
            .await;
        assert!(res2.is_err(), "one waiter must be aborted");
        assert_eq!(res2.unwrap_err().sqlstate(), "40P01");
        // Releasing the victim lets the other proceed.
        lm.release_transaction(s2);
        let _ = tokio::time::timeout(Duration::from_secs(2), h1).await.unwrap().unwrap();
        let _ = b;
    }

    #[tokio::test]
    async fn lock_timeout_fires() {
        let lm = Arc::new(LockManager::new());
        let s1 = lm.new_session();
        let s2 = lm.new_session();
        let obj = LockObject::Table(9);
        lm.acquire(s1, obj.clone(), LockMode::Exclusive, LockScope::Transaction, WaitPolicy::Wait, None).await.unwrap();
        let err = lm
            .acquire(s2, obj, LockMode::Exclusive, LockScope::Transaction, WaitPolicy::Wait, Some(Duration::from_millis(50)))
            .await
            .unwrap_err();
        assert_eq!(err.sqlstate(), "55P03");
    }

    #[test]
    fn snapshot_reports_granted() {
        let lm = LockManager::new();
        let s1 = lm.new_session();
        assert!(lm.try_acquire(s1, LockObject::Table(7), LockMode::RowExclusive, LockScope::Transaction));
        let snap = lm.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].mode, "RowExclusiveLock");
        assert!(snap[0].granted);
    }
}
