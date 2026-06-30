#![cfg(feature = "sql")]
//! Concurrency tests for the lock manager: two sessions sharing a database,
//! exercising blocking, deadlock detection, NOWAIT, SKIP LOCKED, advisory locks,
//! LOCK TABLE, pg_locks, and lock release on commit/rollback.

use std::sync::Arc;
use std::time::Duration;

use guardian_db::sql::engine::{Database, Session};
use guardian_db::sql::{ExecResult, MemoryStorage};

type Db = Arc<Database<MemoryStorage>>;

async fn setup() -> Db {
    let db: Db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let mut s = Session::new(db.clone(), "guardian");
    s.execute("CREATE TABLE t (id INT PRIMARY KEY, v INT)").await.unwrap();
    s.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)").await.unwrap();
    db
}

fn rows(r: &ExecResult) -> &Vec<Vec<guardian_db::sql::SqlValue>> {
    match r {
        ExecResult::Rows { rows, .. } => rows,
        ExecResult::Command { tag } => panic!("expected rows, got {tag}"),
    }
}

async fn one(s: &mut Session<MemoryStorage>, sql: &str) -> ExecResult {
    s.execute(sql).await.unwrap_or_else(|e| panic!("`{sql}`: {e}")).pop().unwrap()
}

#[tokio::test]
async fn for_update_blocks_concurrent_update() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    one(&mut s1, "BEGIN").await;
    one(&mut s1, "SELECT * FROM t WHERE id = 1 FOR UPDATE").await; // holds row 1

    let db2 = db.clone();
    let handle = tokio::spawn(async move {
        let mut s2 = Session::new(db2, "guardian");
        s2.execute("BEGIN").await.unwrap();
        let r = s2.execute("UPDATE t SET v = 99 WHERE id = 1").await; // blocks
        s2.execute("COMMIT").await.unwrap();
        r
    });

    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!handle.is_finished(), "s2 must block on the row lock held by s1");

    one(&mut s1, "COMMIT").await; // release row 1
    let r = tokio::time::timeout(Duration::from_secs(3), handle).await.unwrap().unwrap();
    assert!(r.is_ok(), "s2 should proceed after s1 commits");

    let mut s3 = Session::new(db, "guardian");
    assert_eq!(rows(&one(&mut s3, "SELECT v FROM t WHERE id = 1").await)[0][0].to_text().unwrap(), "99");
}

#[tokio::test]
async fn deadlock_is_detected_and_aborts_a_victim() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    let mut s2 = Session::new(db.clone(), "guardian");
    one(&mut s1, "BEGIN").await;
    one(&mut s2, "BEGIN").await;
    one(&mut s1, "UPDATE t SET v = v + 1 WHERE id = 1").await; // s1 holds row 1
    one(&mut s2, "UPDATE t SET v = v + 1 WHERE id = 2").await; // s2 holds row 2

    let handle = tokio::spawn(async move {
        let r = s1.execute("UPDATE t SET v = v + 1 WHERE id = 2").await; // blocks on row 2
        (s1, r)
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    // s2 now requests row 1, closing the cycle -> deadlock.
    let r2 = s2.execute("UPDATE t SET v = v + 1 WHERE id = 1").await;
    assert!(r2.is_err());
    assert_eq!(r2.unwrap_err().sqlstate(), "40P01");
    one(&mut s2, "ROLLBACK").await; // release the victim's locks

    let (mut s1, r1) = tokio::time::timeout(Duration::from_secs(3), handle).await.unwrap().unwrap();
    assert!(r1.is_ok(), "the survivor proceeds once the victim rolls back");
    one(&mut s1, "COMMIT").await;
}

#[tokio::test]
async fn nowait_fails_when_locked() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    one(&mut s1, "BEGIN").await;
    one(&mut s1, "SELECT * FROM t WHERE id = 1 FOR UPDATE").await;

    let mut s2 = Session::new(db.clone(), "guardian");
    one(&mut s2, "BEGIN").await;
    let err = s2.execute("SELECT * FROM t WHERE id = 1 FOR UPDATE NOWAIT").await.unwrap_err();
    assert_eq!(err.sqlstate(), "55P03");
}

#[tokio::test]
async fn skip_locked_skips_locked_rows() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    one(&mut s1, "BEGIN").await;
    one(&mut s1, "SELECT * FROM t WHERE id = 1 FOR UPDATE").await; // lock row 1

    let mut s2 = Session::new(db.clone(), "guardian");
    one(&mut s2, "BEGIN").await;
    let r = one(&mut s2, "SELECT id FROM t ORDER BY id FOR UPDATE SKIP LOCKED").await;
    let ids: Vec<String> = rows(&r).iter().map(|row| row[0].to_text().unwrap()).collect();
    assert_eq!(ids, vec!["2", "3"], "row 1 (locked by s1) must be skipped");
}

#[tokio::test]
async fn advisory_locks_session_and_try() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    let mut s2 = Session::new(db.clone(), "guardian");

    one(&mut s1, "SELECT pg_advisory_lock(42)").await; // session-level exclusive
    let r = one(&mut s2, "SELECT pg_try_advisory_lock(42)").await;
    assert_eq!(rows(&r)[0][0].to_text().unwrap(), "f", "another session cannot take it");

    one(&mut s1, "SELECT pg_advisory_unlock(42)").await;
    let r = one(&mut s2, "SELECT pg_try_advisory_lock(42)").await;
    assert_eq!(rows(&r)[0][0].to_text().unwrap(), "t", "available after unlock");
}

#[tokio::test]
async fn lock_table_blocks_readers() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    one(&mut s1, "BEGIN").await;
    one(&mut s1, "LOCK TABLE t IN ACCESS EXCLUSIVE MODE").await;

    // A reader with NOWAIT fails immediately under ACCESS EXCLUSIVE.
    let mut s2 = Session::new(db.clone(), "guardian");
    one(&mut s2, "BEGIN").await;
    // Implicit ACCESS SHARE for the SELECT conflicts; use a blocking spawn.
    let db3 = db.clone();
    let handle = tokio::spawn(async move {
        let mut s3 = Session::new(db3, "guardian");
        s3.execute("SELECT count(*) FROM t").await
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!handle.is_finished(), "reader blocks under ACCESS EXCLUSIVE");

    one(&mut s1, "COMMIT").await;
    let r = tokio::time::timeout(Duration::from_secs(3), handle).await.unwrap().unwrap();
    assert!(r.is_ok());
    drop(s2);
}

#[tokio::test]
async fn pg_locks_reports_held_locks() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    one(&mut s1, "BEGIN").await;
    one(&mut s1, "SELECT * FROM t WHERE id = 1 FOR UPDATE").await;

    let mut s2 = Session::new(db.clone(), "guardian");
    let r = one(&mut s2, "SELECT locktype, mode, granted FROM pg_catalog.pg_locks").await;
    let grid = rows(&r);
    assert!(grid.iter().any(|row| row[0].to_text().unwrap() == "tuple"
        && row[1].to_text().unwrap() == "ExclusiveLock"));
}

#[tokio::test]
async fn locks_released_on_rollback() {
    let db = setup().await;
    let mut s1 = Session::new(db.clone(), "guardian");
    one(&mut s1, "BEGIN").await;
    one(&mut s1, "SELECT * FROM t WHERE id = 1 FOR UPDATE").await;
    one(&mut s1, "ROLLBACK").await; // releases the row lock

    // s2 can now lock it immediately (NOWAIT succeeds).
    let mut s2 = Session::new(db.clone(), "guardian");
    one(&mut s2, "BEGIN").await;
    let r = s2.execute("SELECT * FROM t WHERE id = 1 FOR UPDATE NOWAIT").await;
    assert!(r.is_ok(), "lock should be free after rollback");
}
