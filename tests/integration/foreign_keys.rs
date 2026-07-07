use crate::common::{ExecRows, TempDatabase};
use std::sync::Arc;
use turso_core::{Connection, LimboError};

/// Build `t(id, parent_id)` where each row references the previous one via
/// ON DELETE CASCADE: deleting row 1 cascades one subprogram frame per row.
fn create_cascade_chain(conn: &Arc<Connection>, len: usize) {
    conn.execute("PRAGMA foreign_keys=ON").unwrap();
    conn.execute(
        "CREATE TABLE t(
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES t(id) ON DELETE CASCADE
        )",
    )
    .unwrap();
    conn.execute("CREATE INDEX t_parent_idx ON t(parent_id)")
        .unwrap();
    let mut insert = String::from("INSERT INTO t VALUES (1,NULL)");
    for id in 2..=len {
        insert.push_str(&format!(",({id},{})", id - 1));
    }
    conn.execute(&insert).unwrap();
}

fn row_count(conn: &Arc<Connection>) -> i64 {
    let rows: Vec<(i64,)> = conn.exec_rows("SELECT count(*) FROM t");
    rows[0].0
}

fn assert_trigger_recursion_error(result: Result<(), LimboError>) {
    assert!(
        matches!(result, Err(LimboError::TooManyLevelsOfTriggerRecursion)),
        "expected trigger recursion depth error, got {result:?}"
    );
}

/// Regression test for #5154: nested cascade frames used to run on the Rust
/// call stack and overflowed it well below the trigger depth limit.
#[turso_macros::test]
fn fk_cascade_chain_below_depth_limit_completes(db: TempDatabase) {
    let conn = db.connect_limbo();
    let len = (Connection::MAX_TRIGGER_DEPTH - 10) as usize;
    create_cascade_chain(&conn, len);

    conn.execute("DELETE FROM t WHERE id = 1").unwrap();

    assert_eq!(row_count(&conn), 0);
}

/// A chain longer than the depth limit fails with SQLite's bounded error
/// and rolls the statement back.
#[turso_macros::test]
fn fk_cascade_chain_beyond_depth_limit_errors(db: TempDatabase) {
    let conn = db.connect_limbo();
    let len = (Connection::MAX_TRIGGER_DEPTH + 2) as usize;
    create_cascade_chain(&conn, len);

    assert_trigger_recursion_error(conn.execute("DELETE FROM t WHERE id = 1"));

    assert_eq!(row_count(&conn), len as i64);
}

#[turso_macros::test]
fn fk_cascade_respects_lowered_depth_limit(db: TempDatabase) {
    let conn = db.connect_limbo();
    create_cascade_chain(&conn, 8);

    conn.set_limit_trigger_depth(2);
    assert_trigger_recursion_error(conn.execute("DELETE FROM t WHERE id = 1"));
    assert_eq!(row_count(&conn), 8);

    conn.set_limit_trigger_depth(Connection::MAX_TRIGGER_DEPTH);
    conn.execute("DELETE FROM t WHERE id = 1").unwrap();
    assert_eq!(row_count(&conn), 0);
}

#[turso_macros::test]
fn limit_trigger_depth_clamps_to_valid_range(db: TempDatabase) {
    let conn = db.connect_limbo();
    assert_eq!(conn.limit_trigger_depth(), Connection::MAX_TRIGGER_DEPTH);

    conn.set_limit_trigger_depth(-5);
    assert_eq!(conn.limit_trigger_depth(), 0);

    conn.set_limit_trigger_depth(i32::MAX);
    assert_eq!(conn.limit_trigger_depth(), Connection::MAX_TRIGGER_DEPTH);
}

/// A depth limit of zero refuses to run any FK action, matching SQLite's
/// `nFrame >= limit` check.
#[turso_macros::test]
fn fk_cascade_depth_limit_zero_blocks_all_actions(db: TempDatabase) {
    let conn = db.connect_limbo();
    create_cascade_chain(&conn, 2);

    conn.set_limit_trigger_depth(0);
    assert_trigger_recursion_error(conn.execute("DELETE FROM t WHERE id = 1"));
    assert_eq!(row_count(&conn), 2);
}

/// A chain of distinct triggers also counts against the depth limit, and a
/// depth error inside trigger frames must unwind cleanly: the connection
/// stays usable afterwards.
#[turso_macros::test]
fn trigger_chain_respects_depth_limit_and_unwinds(db: TempDatabase) {
    let conn = db.connect_limbo();
    for i in 0..4 {
        conn.execute(&format!("CREATE TABLE t{i}(x INTEGER PRIMARY KEY)"))
            .unwrap();
    }
    for i in 0..3 {
        conn.execute(&format!(
            "CREATE TRIGGER tr{i} AFTER INSERT ON t{i} BEGIN
                 INSERT INTO t{}(x) VALUES (NEW.x);
             END",
            i + 1
        ))
        .unwrap();
    }

    conn.set_limit_trigger_depth(2);
    assert_trigger_recursion_error(conn.execute("INSERT INTO t0 VALUES (1)"));

    conn.set_limit_trigger_depth(Connection::MAX_TRIGGER_DEPTH);
    conn.execute("INSERT INTO t0 VALUES (2)").unwrap();
    for i in 0..4 {
        let rows: Vec<(i64,)> = conn.exec_rows(&format!("SELECT count(*) FROM t{i}"));
        assert_eq!(rows, vec![(1,)], "t{i} should hold only the second insert");
    }
}

/// Resetting a statement mid-cascade must tear down live subprogram frames
/// without corrupting the connection: an unfinished delete rolls back, and
/// the chain can still be deleted afterwards.
#[turso_macros::test]
fn fk_cascade_reset_mid_cascade_rolls_back(db: TempDatabase) {
    let conn = db.connect_limbo();
    create_cascade_chain(&conn, 100);

    let mut stmt = conn.prepare("DELETE FROM t WHERE id = 1").unwrap();
    // A single step may or may not finish the cascade depending on how IO
    // completes; both outcomes must leave the connection consistent.
    let done = matches!(stmt.step().unwrap(), turso_core::StepResult::Done);
    stmt.reset().unwrap();
    drop(stmt);

    if done {
        assert_eq!(row_count(&conn), 0);
    } else {
        assert_eq!(row_count(&conn), 100, "abandoned delete must roll back");
        conn.execute("DELETE FROM t WHERE id = 1").unwrap();
        assert_eq!(row_count(&conn), 0);
    }
}
