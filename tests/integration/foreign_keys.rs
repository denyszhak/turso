use crate::common::{limbo_exec_rows, TempDatabase};
use anyhow::Result;
use rusqlite::types::Value as SqlValue;
use std::sync::Arc;
use turso_core::{Connection, LimboError};

fn create_self_referencing_chain(conn: &Arc<Connection>, len: usize) -> Result<()> {
    conn.execute("PRAGMA foreign_keys=ON")?;
    conn.execute(
        "CREATE TABLE t(
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES t(id) ON DELETE CASCADE
        )",
    )?;
    conn.execute("BEGIN")?;
    for id in 1..=len {
        let sql = if id == 1 {
            "INSERT INTO t VALUES (1, NULL)".to_string()
        } else {
            format!("INSERT INTO t VALUES ({id}, {})", id - 1)
        };
        conn.execute(&sql)?;
    }
    conn.execute("COMMIT")?;
    Ok(())
}

fn assert_count(conn: &Arc<Connection>, expected: i64) {
    let rows = limbo_exec_rows(conn, "SELECT count(*) FROM t");
    assert_eq!(rows.len(), 1, "expected a single count row");
    assert_eq!(rows[0].len(), 1, "expected a single count column");
    match rows[0].first() {
        Some(SqlValue::Integer(actual)) => assert_eq!(*actual, expected),
        other => panic!("expected integer count result, got {other:?}"),
    }
}

#[turso_macros::test()]
fn fk_cascade_delete_self_reference_honors_runtime_limit(tmp_db: TempDatabase) -> Result<()> {
    let conn = tmp_db.connect_limbo();
    conn.set_trigger_recursion_limit(2);
    create_self_referencing_chain(&conn, 4)?;

    let result = conn.execute("DELETE FROM t WHERE id = 1");
    assert!(
        matches!(result, Err(LimboError::ParseError(ref msg)) if msg.contains("too many levels of trigger recursion")),
        "expected bounded recursion error, got {result:?}"
    );
    assert_count(&conn, 4);

    Ok(())
}

#[turso_macros::test()]
fn fk_cascade_delete_self_reference_hits_default_depth_limit(tmp_db: TempDatabase) -> Result<()> {
    let conn = tmp_db.connect_limbo();
    let chain_len = (Connection::MAX_TRIGGER_RECURSION_DEPTH + 2) as usize;
    create_self_referencing_chain(&conn, chain_len)?;

    let result = conn.execute("DELETE FROM t WHERE id = 1");
    assert!(
        matches!(result, Err(LimboError::ParseError(ref msg)) if msg.contains("too many levels of trigger recursion")),
        "expected bounded recursion error, got {result:?}"
    );
    assert_count(&conn, chain_len as i64);

    Ok(())
}
