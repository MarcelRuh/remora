//! Versionierte Schema-Migrationen (über `PRAGMA user_version`).

use rusqlite::Connection;

/// Jede Migration wird genau einmal in einer Transaktion ausgeführt.
const MIGRATIONS: &[&str] = &[
    // v1: Grundschema
    r#"
    CREATE TABLE folders (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        name        TEXT    NOT NULL,
        parent_id   INTEGER REFERENCES folders(id) ON DELETE CASCADE,
        sort_order  INTEGER NOT NULL DEFAULT 0,
        expanded    INTEGER NOT NULL DEFAULT 1
    );

    CREATE TABLE connections (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        name        TEXT    NOT NULL,
        host        TEXT    NOT NULL,
        port        INTEGER NOT NULL DEFAULT 3389,
        username    TEXT    NOT NULL DEFAULT '',
        domain      TEXT    NOT NULL DEFAULT '',
        description TEXT    NOT NULL DEFAULT '',
        folder_id   INTEGER REFERENCES folders(id) ON DELETE SET NULL,
        tags        TEXT    NOT NULL DEFAULT '[]',
        color       TEXT    NOT NULL DEFAULT '#4c8dff',
        icon        TEXT    NOT NULL DEFAULT 'monitor',
        favorite    INTEGER NOT NULL DEFAULT 0,
        options     TEXT    NOT NULL DEFAULT '{}',
        sort_order  INTEGER NOT NULL DEFAULT 0,
        last_used   TEXT,
        created_at  TEXT    NOT NULL,
        updated_at  TEXT    NOT NULL
    );
    CREATE INDEX idx_connections_folder ON connections(folder_id);
    CREATE INDEX idx_connections_last_used ON connections(last_used);

    CREATE TABLE known_hosts (
        endpoint    TEXT PRIMARY KEY,
        fingerprint TEXT NOT NULL,
        first_seen  TEXT NOT NULL,
        last_seen   TEXT NOT NULL
    );
    "#,
    // v2: Protokoll je Verbindung (RDP oder SSH)
    r#"
    ALTER TABLE connections ADD COLUMN protocol TEXT NOT NULL DEFAULT 'rdp';
    "#,
];

pub fn current_version() -> u32 {
    MIGRATIONS.len() as u32
}

pub fn run(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;

    let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let target = index as u32 + 1;
        tracing::info!(target, "Datenbank-Migration wird angewendet");
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
    }
    Ok(())
}
